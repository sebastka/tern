#include "messagelistmodel.h"

#include <QApplication>
#include <QDateTime>
#include <QFont>
#include <QIcon>
#include <QLocale>
#include <QMimeData>
#include <QPalette>

namespace tern {

MessageListModel::MessageListModel(QObject *parent) : QAbstractTableModel(parent) {}

void MessageListModel::open(const QString &account, qint64 folder, bool threaded, const QString &query,
                            bool showRecipients)
{
    beginResetModel();
    m_pages.clear();
    m_pageOrder.clear();
    m_showRecipients = showRecipients;
    if (account.isEmpty()) {
        core().close_list();
        m_count = 0;
    } else {
        m_count = core().open_list(rs(account), folder, threaded, rs(query));
    }
    endResetModel();
}

void MessageListModel::reload(quint32 count)
{
    beginResetModel();
    m_pages.clear();
    m_pageOrder.clear();
    m_count = count;
    endResetModel();
}

const Row *MessageListModel::rowAt(int row) const
{
    if (row < 0 || static_cast<quint32>(row) >= m_count)
        return nullptr;
    const int page = row / PageSize;
    auto it = m_pages.find(page);
    if (it == m_pages.end()) {
        QList<Row> rows;
        const auto fetched = core().list_rows(static_cast<quint32>(page * PageSize), PageSize);
        rows.reserve(static_cast<qsizetype>(fetched.size()));
        for (const auto &r : fetched) {
            rows << Row{Key::from(r.key), r.depth,     r.thread_size, r.date,       qs(r.from),
                        qs(r.to),         qs(r.subject), r.unread,    r.flagged,    r.answered,
                        r.has_attachments, r.encrypted, r.size};
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
    return parent.isValid() ? 0 : static_cast<int>(m_count);
}

int MessageListModel::columnCount(const QModelIndex &parent) const { return parent.isValid() ? 0 : ColumnCount; }

static QString formatDate(qint64 ts)
{
    const QDateTime dt = QDateTime::fromSecsSinceEpoch(ts).toLocalTime();
    const QDate today = QDate::currentDate();
    const QLocale locale;
    if (dt.date() == today)
        return locale.toString(dt.time(), QLocale::ShortFormat);
    if (dt.date().year() == today.year() && dt.date().daysTo(today) < 7)
        return locale.toString(dt, QStringLiteral("ddd ")) + locale.toString(dt.time(), QLocale::ShortFormat);
    return locale.toString(dt.date(), QLocale::ShortFormat);
}

QVariant MessageListModel::data(const QModelIndex &index, int role) const
{
    const Row *r = rowAt(index.row());
    if (!r)
        return {};
    const int col = index.column();
    switch (role) {
    case Qt::DisplayRole:
        switch (col) {
        case ColSubject: {
            QString s = r->subject.isEmpty() ? tr("(no subject)") : r->subject;
            if (r->depth > 0)
                s.prepend(QString(static_cast<qsizetype>(qMin<quint32>(r->depth, 12)) * 3, u' ') + QStringLiteral("↳ "));
            if (r->threadSize > 1)
                s += QStringLiteral("  [%1]").arg(r->threadSize);
            return s;
        }
        case ColCorrespondent:
            return m_showRecipients ? r->to : r->from;
        case ColDate:
            return formatDate(r->date);
        case ColFlag:
            // Glyphs when the icon theme has no mail icons (e.g. bare WMs).
            if (!QIcon::hasThemeIcon(QStringLiteral("mail-unread"))) {
                if (r->flagged)
                    return QStringLiteral("★");
                if (r->unread)
                    return QStringLiteral("●");
                if (r->answered)
                    return QStringLiteral("↩");
            }
            return {};
        case ColAttachment:
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
        if (col == ColFlag) {
            if (r->flagged)
                return QIcon::fromTheme(QStringLiteral("flag"), QIcon::fromTheme(QStringLiteral("emblem-important")));
            if (r->unread)
                return QIcon::fromTheme(QStringLiteral("mail-unread"));
            if (r->answered)
                return QIcon::fromTheme(QStringLiteral("mail-replied"));
        }
        if (col == ColAttachment) {
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
        if (col == ColFlag || col == ColAttachment)
            return Qt::AlignCenter;
        return {};
    case Qt::ForegroundRole:
        if (r->flagged && (col == ColSubject || col == ColFlag))
            return QColor(0xc0, 0x39, 0x2b);
        return {};
    case Qt::ToolTipRole:
        if (col == ColDate)
            return QLocale().toString(QDateTime::fromSecsSinceEpoch(r->date).toLocalTime(), QLocale::LongFormat);
        if (col == ColSubject)
            return r->subject;
        if (col == ColCorrespondent)
            return tr("From: %1\nTo: %2").arg(r->from, r->to);
        return {};
    default:
        return {};
    }
}

QVariant MessageListModel::headerData(int section, Qt::Orientation orientation, int role) const
{
    if (orientation != Qt::Horizontal)
        return {};
    if (role == Qt::DisplayRole) {
        switch (section) {
        case ColSubject:
            return tr("Subject");
        case ColCorrespondent:
            return m_showRecipients ? tr("To") : tr("From");
        case ColDate:
            return tr("Date");
        default:
            return {};
        }
    }
    if (role == Qt::DecorationRole) {
        if (section == ColFlag)
            return QIcon::fromTheme(QStringLiteral("flag"));
        if (section == ColAttachment)
            return QIcon::fromTheme(QStringLiteral("mail-attachment"));
    }
    return {};
}

Qt::ItemFlags MessageListModel::flags(const QModelIndex &index) const
{
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
