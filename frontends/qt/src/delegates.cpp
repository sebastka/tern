#include "delegates.h"

#include <QApplication>
#include <QHash>
#include <QPainter>
#include <QPainterPath>

namespace tern {

BadgeDelegate::BadgeDelegate(int countRole, QObject *parent) : QStyledItemDelegate(parent), m_countRole(countRole) {}

static QString badgeText(quint32 count) { return count > 9999 ? QStringLiteral("9999+") : QString::number(count); }

static QFont badgeFont(const QFont &base)
{
    QFont f = base;
    f.setBold(true);
    f.setPointSizeF(f.pointSizeF() * 0.85);
    return f;
}

QSize BadgeDelegate::badgeSize(const QStyleOptionViewItem &option, quint32 count) const
{
    const QFontMetrics fm(badgeFont(option.font));
    const int h = fm.height() + 2;
    // At least round: a single digit is a circle.
    return {qMax(h, fm.horizontalAdvance(badgeText(count)) + h / 2 + 4), h};
}

void BadgeDelegate::paint(QPainter *painter, const QStyleOptionViewItem &option, const QModelIndex &index) const
{
    QStyleOptionViewItem opt(option);
    initStyleOption(&opt, index);
    const quint32 count = index.data(m_countRole).toUInt();
    const QStyle *style = opt.widget ? opt.widget->style() : QApplication::style();
    if (count == 0) {
        style->drawControl(QStyle::CE_ItemViewItem, &opt, painter, opt.widget);
        return;
    }

    // Leave room for the badge: the name is elided before it.
    const QSize badge = badgeSize(opt, count);
    const int margin = 6;
    const QRect textRect = style->subElementRect(QStyle::SE_ItemViewItemText, &opt, opt.widget);
    const int available = textRect.width() - badge.width() - 2 * margin;
    opt.text = opt.fontMetrics.elidedText(opt.text, Qt::ElideRight, qMax(0, available));
    style->drawControl(QStyle::CE_ItemViewItem, &opt, painter, opt.widget);

    const QRect r(opt.rect.right() - margin - badge.width(), opt.rect.center().y() - badge.height() / 2 + 1,
                  badge.width(), badge.height());
    const bool selected = opt.state & QStyle::State_Selected;
    const QPalette &pal = opt.palette;
    // On a selected row the colors swap, so the badge stays visible.
    const QColor bg = selected ? pal.color(QPalette::HighlightedText) : pal.color(QPalette::Highlight);
    const QColor fg = selected ? pal.color(QPalette::Highlight) : pal.color(QPalette::HighlightedText);
    painter->save();
    painter->setRenderHint(QPainter::Antialiasing);
    painter->setPen(Qt::NoPen);
    painter->setBrush(bg);
    painter->drawRoundedRect(r, r.height() / 2.0, r.height() / 2.0);
    painter->setPen(fg);
    painter->setFont(badgeFont(opt.font));
    painter->drawText(r, Qt::AlignCenter, badgeText(count));
    painter->restore();
}

QSize BadgeDelegate::sizeHint(const QStyleOptionViewItem &option, const QModelIndex &index) const
{
    QSize s = QStyledItemDelegate::sizeHint(option, index);
    const quint32 count = index.data(m_countRole).toUInt();
    if (count > 0)
        s.rwidth() += badgeSize(option, count).width() + 12;
    return s;
}

QIcon unreadDot(const QPalette &palette, bool unread)
{
    // Cached per color: the list asks for it on every repaint.
    static QHash<QString, QIcon> cache;
    static QIcon none;
    const int size = 16;
    if (!unread) {
        if (none.isNull()) {
            QPixmap pm(size, size);
            pm.fill(Qt::transparent);
            none = QIcon(pm);
        }
        return none;
    }
    const QColor color = palette.color(QPalette::Highlight);
    // A ring in the base color keeps the dot visible on a selected row,
    // whose background is the same accent color.
    const QColor ring = palette.color(QPalette::Base);
    const QString key = color.name() + ring.name();
    auto it = cache.find(key);
    if (it == cache.end()) {
        QPixmap pm(size * 2, size * 2); // drawn at 2x, crisp on HiDPI
        pm.setDevicePixelRatio(2);
        pm.fill(Qt::transparent);
        QPainter p(&pm);
        p.setRenderHint(QPainter::Antialiasing);
        p.setPen(QPen(ring, 1.5));
        p.setBrush(color);
        p.drawEllipse(QRectF(size / 2.0 - 4.5, size / 2.0 - 4.5, 9, 9));
        p.end();
        it = cache.insert(key, QIcon(pm));
    }
    return *it;
}

} // namespace tern
