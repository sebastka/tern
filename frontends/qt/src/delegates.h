// Item delegates that only change presentation.
#pragma once

#include <QIcon>
#include <QStyledItemDelegate>

namespace tern {

// Folder tree: the unread count as a rounded badge at the right edge,
// instead of "Name (12)" in the text. The count comes from `countRole`.
class BadgeDelegate : public QStyledItemDelegate {
    Q_OBJECT
public:
    BadgeDelegate(int countRole, QObject *parent = nullptr);

    void paint(QPainter *painter, const QStyleOptionViewItem &option, const QModelIndex &index) const override;
    QSize sizeHint(const QStyleOptionViewItem &option, const QModelIndex &index) const override;

private:
    QSize badgeSize(const QStyleOptionViewItem &option, quint32 count) const;
    int m_countRole;
};

// A small filled dot in the accent color (unread marker), or a transparent
// icon of the same size, so read and unread rows keep their text aligned.
QIcon unreadDot(const QPalette &palette, bool unread);

} // namespace tern
