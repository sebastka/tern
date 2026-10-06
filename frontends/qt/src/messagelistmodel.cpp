#include "messagelistmodel.h"

#include "delegates.h"

#include <QApplication>
#include <algorithm>
#include <QDateTime>
#include <QFont>
#include <QIcon>
#include <QLocale>
#include <QMimeData>
#include <QPalette>

namespace tern {

MessageListModel::MessageListModel(QObject *parent) : QAbstractTableModel(parent) {}

void MessageListModel::setColumns(const QList<Column> &columns)
{
    if (columns == m_columns)
        return;
    beginResetModel();
    m_columns = columns;
    endResetModel();
}

void MessageListModel::open(const QString &account, qint64 folder, bool threaded, const QString &query,
                            bool showRecipients, ffi::ListFilter filter)
{
    beginResetModel();
    m_pages.clear();
    m_pageOrder.clear();
    m_showRecipients = showRecipients;
    if (account.isEmpty()) {
        core().close_list();
        m_count = 0;
    } else {
        m_count = core().open_list(rs(account), folder, threaded, rs(query), filter);
    }
    loadGroups();
    endResetModel();
}

void MessageListModel::reload(quint32 count)
{
    beginResetModel();
    m_pages.clear();
    m_pageOrder.clear();
    m_count = count;
    loadGroups();
    endResetModel();
}

static QString groupLabel(const ffi::ListGroup &g)
{
    switch (g.kind) {
    case ffi::DateGroup::Today:
        return MessageListModel::tr("Today");
    case ffi::DateGroup::Yesterday:
        return MessageListModel::tr("Yesterday");
    case ffi::DateGroup::ThisWeek:
        return MessageListModel::tr("Earlier this week");
    case ffi::DateGroup::LastWeek:
        return MessageListModel::tr("Last week");
    default: {
        // "September", or "September 2025" for other years.
        QString month = QLocale().standaloneMonthName(static_cast<int>(g.month));
        if (!month.isEmpty())
            month[0] = month[0].toUpper();
        return g.year == QDate::currentDate().year() ? month : QStringLiteral("%1 %2").arg(month).arg(g.year);
    }
    }
}

void MessageListModel::loadGroups()
{
    m_headers.clear();
    m_groupStarts.clear();
    m_headerLabels.clear();
    if (m_count == 0)
        return;
    const auto groups = core().list_groups();
    for (const auto &g : groups) {
        m_headers << static_cast<int>(g.start) + static_cast<int>(m_headers.size());
        m_groupStarts << g.start;
        m_headerLabels << groupLabel(g);
    }
}

qint64 MessageListModel::coreIndex(int row) const
{
    // Headers at or before `row`.
    const auto k = std::upper_bound(m_headers.cbegin(), m_headers.cend(), row) - m_headers.cbegin();
    if (k > 0 && m_headers[k - 1] == row)
        return -1;
    const qint64 i = row - k;
    return i >= 0 && i < m_count ? i : -1;
}

int MessageListModel::viewRow(qint64 coreIndex) const
{
    if (coreIndex < 0 || coreIndex >= m_count)
        return -1;
    // Sections starting at or before the message.
    const auto k = std::upper_bound(m_groupStarts.cbegin(), m_groupStarts.cend(), coreIndex) - m_groupStarts.cbegin();
    return static_cast<int>(coreIndex + k);
}

const Row *MessageListModel::rowAt(int viewRow) const
{
    const qint64 row = coreIndex(viewRow);
    if (row < 0)
        return nullptr;
    const int page = row / PageSize;
    auto it = m_pages.find(page);
    if (it == m_pages.end()) {
        QList<Row> rows;
        const auto fetched = core().list_rows(static_cast<quint32>(page * PageSize), PageSize);
        rows.reserve(static_cast<qsizetype>(fetched.size()));
        for (const auto &r : fetched) {
            rows << Row{Key::from(r.key), r.depth,     r.thread_size, r.date,        qs(r.from),
                        qs(r.to),         qs(r.subject), r.unread,    r.flagged,     r.answered,
                        r.forwarded,      r.has_attachments, r.encrypted, r.size};
        }
        if (m_pageOrder.size() >= MaxPages)
            m_pages.remove(m_pageOrder.takeFirst());
        it = m_pages.insert(page, std::move(rows));
        m_pageOrder << page;
    } else {
        m_pageOrder.removeOne(page);
        m_pageOrder << page;
    }
    const int i = row % PageSize;
    return i < it->size() ? &it->at(i) : nullptr;
}

Key MessageListModel::keyAt(int row) const
{
    const Row *r = rowAt(row);
    return r ? r->key : Key{};
}

int MessageListModel::rowCount(const QModelIndex &parent) const
{
    return parent.isValid() ? 0 : static_cast<int>(m_count + m_headers.size());
}

int MessageListModel::columnCount(const QModelIndex &parent) const
{
    return parent.isValid() ? 0 : static_cast<int>(m_columns.size());
}

// With date sections, "Today" and "Yesterday" rows only need the time.
static QString formatDate(qint64 ts, bool sectioned)
{
    const QDateTime dt = QDateTime::fromSecsSinceEpoch(ts).toLocalTime();
    const QDate today = QDate::currentDate();
    const QLocale locale;
    if (dt.date() == today || (sectioned && dt.date() == today.addDays(-1)))
        return locale.toString(dt.time(), QLocale::ShortFormat);
    if (dt.date().year() == today.year() && dt.date().daysTo(today) < 7)
        return locale.toString(dt, QStringLiteral("ddd ")) + locale.toString(dt.time(), QLocale::ShortFormat);
    return locale.toString(dt.date(), QLocale::ShortFormat);
}

QVariant MessageListModel::data(const QModelIndex &index, int role) const
{
    if (coreIndex(index.row()) < 0) {
        // A date section header, spanned across the row by the view.
        const qsizetype h = m_headers.indexOf(index.row());
        if (h < 0 || index.column() != 0)
            return {};
        switch (role) {
        case Qt::DisplayRole:
            return m_headerLabels.value(h);
        case Qt::FontRole: {
            QFont f;
            f.setBold(true);
            f.setPointSizeF(f.pointSizeF() * 0.9);
            return f;
        }
        case Qt::ForegroundRole:
            return QApplication::palette().color(QPalette::PlaceholderText);
        default:
            return {};
        }
    }
    const Row *r = rowAt(index.row());
    if (!r)
        return {};
    const Column col = columnAt(index.column());
    switch (role) {
    case Qt::DisplayRole:
        switch (col) {
        case Column::Subject: {
            QString s = r->subject.isEmpty() ? tr("(no subject)") : r->subject;
            if (r->depth > 0)
                s.prepend(QString(static_cast<qsizetype>(qMin<quint32>(r->depth, 12)) * 3, u' ') + QStringLiteral("↳ "));
            if (r->threadSize > 1)
                s += QStringLiteral("  [%1]").arg(r->threadSize);
            return s;
        }
        case Column::Correspondent:
            return m_showRecipients ? r->to : r->from;
        case Column::From:
            return r->from;
        case Column::To:
            return r->to;
        case Column::Date:
            return formatDate(r->date, !m_headers.isEmpty());
        case Column::Size:
            return QLocale().formattedDataSize(r->size, 0);
        case Column::Flag:
            // Glyphs when the icon theme has no mail icons (e.g. bare WMs).
            // Unread is shown by the subject's dot and bold text instead.
            if (!QIcon::hasThemeIcon(QStringLiteral("mail-replied"))) {
                if (r->flagged)
                    return QStringLiteral("★");
                if (r->answered && r->forwarded)
                    return QStringLiteral("⇄");
                if (r->answered)
                    return QStringLiteral("↩");
                if (r->forwarded)
                    return QStringLiteral("↪");
            }
            return {};
        case Column::Attachment:
            if (!QIcon::hasThemeIcon(QStringLiteral("mail-attachment"))) {
                if (r->encrypted)
                    return QStringLiteral("🔒");
                if (r->hasAttachments)
                    return QStringLiteral("📎");
            }
            return {};
        default:
            return {};
        }
    case Qt::DecorationRole:
        // One unread signal: an accent dot (plus bold text), not an icon on
        // every unread row.
        if (col == Column::Subject)
            return unreadDot(QApplication::palette(), r->unread);
        if (col == Column::Flag) {
            if (r->flagged)
                return QIcon::fromTheme(QStringLiteral("flag"), QIcon::fromTheme(QStringLiteral("emblem-important")));
            // mail-forwarded(-replied) are Breeze names; the spec only has
            // mail-replied and the mail-forward action.
            if (r->answered && r->forwarded)
                return QIcon::fromTheme(QStringLiteral("mail-forwarded-replied"),
                                        QIcon::fromTheme(QStringLiteral("mail-replied")));
            if (r->answered)
                return QIcon::fromTheme(QStringLiteral("mail-replied"));
            if (r->forwarded)
                return QIcon::fromTheme(QStringLiteral("mail-forwarded"),
                                        QIcon::fromTheme(QStringLiteral("mail-forward")));
        }
        if (col == Column::Attachment) {
            if (r->encrypted)
                return QIcon::fromTheme(QStringLiteral("document-encrypted"));
            if (r->hasAttachments)
                return QIcon::fromTheme(QStringLiteral("mail-attachment"));
        }
        return {};
    case Qt::FontRole:
        if (r->unread) {
            QFont f;
            f.setBold(true);
            return f;
        }
        return {};
    case Qt::TextAlignmentRole:
        if (col == Column::Flag || col == Column::Attachment)
            return Qt::AlignCenter;
        if (col == Column::Size)
            return QVariant::fromValue(Qt::AlignRight | Qt::AlignVCenter);
        return {};
    case Qt::ForegroundRole:
        if (r->flagged && (col == Column::Subject || col == Column::Flag))
            return QColor(0xc0, 0x39, 0x2b);
        return {};
    case Qt::ToolTipRole:
        if (col == Column::Date)
            return QLocale().toString(QDateTime::fromSecsSinceEpoch(r->date).toLocalTime(), QLocale::LongFormat);
        if (col == Column::Subject)
            return r->subject;
        if (col == Column::Correspondent || col == Column::From || col == Column::To)
            return tr("From: %1\nTo: %2").arg(r->from, r->to);
        if (col == Column::Size)
            return QLocale().toString(r->size);
        if (col == Column::Flag) {
            // One icon shows; the tooltip lists every state.
            QStringList states;
            if (r->flagged)
                states << tr("Flagged");
            if (r->unread)
                states << tr("Unread");
            if (r->answered)
                states << tr("Answered");
            if (r->forwarded)
                states << tr("Forwarded");
            return states.isEmpty() ? QVariant() : QVariant(states.join(u'\n'));
        }
        return {};
    default:
        return {};
    }
}

QVariant MessageListModel::headerData(int section, Qt::Orientation orientation, int role) const
{
    if (orientation != Qt::Horizontal || section < 0 || section >= m_columns.size())
        return {};
    const Column col = m_columns[section];
    if (role == Qt::DisplayRole) {
        switch (col) {
        case Column::Subject:
            return tr("Subject");
        case Column::Correspondent:
            return m_showRecipients ? tr("To") : tr("From");
        case Column::From:
            return tr("From");
        case Column::To:
            return tr("To");
        case Column::Date:
            return tr("Date");
        case Column::Size:
            return tr("Size");
        default:
            return {};
        }
    }
    if (role == Qt::DecorationRole) {
        if (col == Column::Flag)
            return QIcon::fromTheme(QStringLiteral("flag"));
        if (col == Column::Attachment)
            return QIcon::fromTheme(QStringLiteral("mail-attachment"));
    }
    if (role == Qt::ToolTipRole) {
        if (col == Column::Flag)
            return tr("Flagged, unread, answered or forwarded");
        if (col == Column::Attachment)
            return tr("Attachments or encrypted");
    }
    return {};
}

Qt::ItemFlags MessageListModel::flags(const QModelIndex &index) const
{
    // Section headers can't be selected, and keyboard navigation skips them.
    if (index.isValid() && coreIndex(index.row()) < 0)
        return Qt::NoItemFlags;
    return QAbstractTableModel::flags(index) | Qt::ItemIsDragEnabled;
}

QStringList MessageListModel::mimeTypes() const { return {QString::fromLatin1(MimeType)}; }

Qt::DropActions MessageListModel::supportedDragActions() const { return Qt::MoveAction; }

QMimeData *MessageListModel::mimeData(const QModelIndexList &indexes) const
{
    QByteArray payload;
    QList<int> seen;
    for (const QModelIndex &i : indexes) {
        if (seen.contains(i.row()))
            continue;
        seen << i.row();
        const Key k = keyAt(i.row());
        if (k.valid())
            payload += k.account.toUtf8() + '\t' + QByteArray::number(k.id) + '\n';
    }
    auto *data = new QMimeData;
    data->setData(QString::fromLatin1(MimeType), payload);
    return data;
}

QList<Key> MessageListModel::keysFromMime(const QMimeData *data)
{
    QList<Key> keys;
    const QByteArray payload = data->data(QString::fromLatin1(MimeType));
    for (const QByteArray &line : payload.split('\n')) {
        const qsizetype tab = line.indexOf('\t');
        if (tab <= 0)
            continue;
        bool ok = false;
        const qint64 id = line.mid(tab + 1).toLongLong(&ok);
        if (ok)
            keys << Key{QString::fromUtf8(line.left(tab)), id};
    }
    return keys;
}

} // namespace tern
