#include "foldermodel.h"

#include "messagelistmodel.h"

#include <QApplication>
#include <QFont>
#include <QIcon>
#include <QMimeData>
#include <QStyle>

namespace tern {

FolderModel::FolderModel(QObject *parent) : QAbstractItemModel(parent) {}

void FolderModel::refresh()
{
    QList<Node> nodes;
    QList<int> roots;
    const auto tree = core().folder_tree();
    nodes.reserve(static_cast<qsizetype>(tree.size()));
    for (const auto &n : tree) {
        Node node;
        node.id = FolderId{qs(n.account), n.folder};
        node.parent = n.parent;
        node.name = qs(n.name);
        node.path = qs(n.path);
        node.role = qs(n.role);
        node.selectable = n.selectable;
        node.unread = n.unread;
        node.total = n.total;
        const int self = static_cast<int>(nodes.size());
        if (n.parent < 0) {
            node.row = static_cast<int>(roots.size());
            roots << self;
        } else {
            Node &p = nodes[n.parent];
            node.row = static_cast<int>(p.children.size());
            p.children << self;
        }
        nodes << node;
    }

    // Same structure → only counts changed: update in place so the view keeps
    // its selection and expansion state.
    bool sameShape = nodes.size() == m_nodes.size();
    for (qsizetype i = 0; sameShape && i < nodes.size(); ++i)
        sameShape = nodes[i].id == m_nodes[i].id && nodes[i].parent == m_nodes[i].parent
                    && nodes[i].name == m_nodes[i].name && nodes[i].role == m_nodes[i].role;
    if (sameShape) {
        for (qsizetype i = 0; i < nodes.size(); ++i) {
            if (nodes[i].unread != m_nodes[i].unread || nodes[i].total != m_nodes[i].total) {
                m_nodes[i].unread = nodes[i].unread;
                m_nodes[i].total = nodes[i].total;
                const QModelIndex idx = createIndex(m_nodes[i].row, 0, quintptr(i));
                Q_EMIT dataChanged(idx, idx);
            }
        }
        return;
    }
    beginResetModel();
    m_nodes = std::move(nodes);
    m_roots = std::move(roots);
    endResetModel();
}

int FolderModel::nodeOf(const QModelIndex &index) const
{
    return index.isValid() ? static_cast<int>(index.internalId()) : -1;
}

FolderId FolderModel::idAt(const QModelIndex &index) const
{
    const int n = nodeOf(index);
    return n >= 0 && n < m_nodes.size() ? m_nodes[n].id : FolderId{};
}

QModelIndex FolderModel::indexOf(const FolderId &id) const
{
    for (qsizetype i = 0; i < m_nodes.size(); ++i)
        if (m_nodes[i].id == id)
            return createIndex(m_nodes[i].row, 0, quintptr(i));
    return {};
}

QModelIndex FolderModel::firstInbox() const
{
    for (qsizetype i = 0; i < m_nodes.size(); ++i)
        if (m_nodes[i].role == u"inbox")
            return createIndex(m_nodes[i].row, 0, quintptr(i));
    return {};
}

QModelIndex FolderModel::index(int row, int column, const QModelIndex &parent) const
{
    if (column != 0 || row < 0)
        return {};
    const int p = nodeOf(parent);
    const QList<int> &siblings = p < 0 ? m_roots : m_nodes[p].children;
    if (row >= siblings.size())
        return {};
    return createIndex(row, 0, quintptr(siblings[row]));
}

QModelIndex FolderModel::parent(const QModelIndex &child) const
{
    const int n = nodeOf(child);
    if (n < 0 || m_nodes[n].parent < 0)
        return {};
    const int p = m_nodes[n].parent;
    return createIndex(m_nodes[p].row, 0, quintptr(p));
}

int FolderModel::rowCount(const QModelIndex &parent) const
{
    if (parent.column() > 0)
        return 0;
    const int p = nodeOf(parent);
    return static_cast<int>(p < 0 ? m_roots.size() : m_nodes[p].children.size());
}

int FolderModel::columnCount(const QModelIndex &) const { return 1; }

static QIcon roleIcon(const QString &role, bool isAccount)
{
    QString name;
    if (isAccount)
        name = QStringLiteral("mail-message");
    else if (role == u"inbox")
        name = QStringLiteral("mail-folder-inbox");
    else if (role == u"sent")
        name = QStringLiteral("mail-folder-sent");
    else if (role == u"drafts")
        name = QStringLiteral("document-edit");
    else if (role == u"trash")
        name = QStringLiteral("user-trash");
    else if (role == u"junk")
        name = QStringLiteral("mail-mark-junk");
    else if (role == u"archive")
        name = QStringLiteral("folder-documents");
    else
        name = QStringLiteral("folder");
    return QIcon::fromTheme(name, QApplication::style()->standardIcon(QStyle::SP_DirIcon));
}

QVariant FolderModel::data(const QModelIndex &index, int role) const
{
    const int n = nodeOf(index);
    if (n < 0)
        return {};
    const Node &node = m_nodes[n];
    const bool isAccount = node.parent < 0;
    switch (role) {
    case Qt::DisplayRole:
        // The unread count is drawn as a badge (BadgeDelegate, UnreadRole).
        return node.name;
    case Qt::ToolTipRole:
        return isAccount ? node.name
                         : tr("%1\n%2 messages, %3 unread").arg(node.path).arg(node.total).arg(node.unread);
    case Qt::DecorationRole:
        return roleIcon(node.role, isAccount);
    case Qt::FontRole:
        if (node.unread > 0 || isAccount) {
            QFont f;
            f.setBold(true);
            return f;
        }
        return {};
    case AccountRole:
        return node.id.account;
    case FolderRole:
        return node.id.folder;
    case SelectableRole:
        return node.selectable;
    case FolderRoleName:
        return node.role;
    case UnreadRole:
        return node.unread;
    default:
        return {};
    }
}

Qt::ItemFlags FolderModel::flags(const QModelIndex &index) const
{
    const int n = nodeOf(index);
    if (n < 0)
        return Qt::NoItemFlags;
    Qt::ItemFlags f = Qt::ItemIsEnabled;
    if (m_nodes[n].selectable)
        f |= Qt::ItemIsSelectable | Qt::ItemIsDropEnabled;
    return f;
}

QStringList FolderModel::mimeTypes() const { return {QString::fromLatin1(MessageListModel::MimeType)}; }

Qt::DropActions FolderModel::supportedDropActions() const { return Qt::MoveAction; }

bool FolderModel::canDropMimeData(const QMimeData *data, Qt::DropAction, int, int, const QModelIndex &parent) const
{
    const int n = nodeOf(parent);
    if (n < 0 || !m_nodes[n].selectable || !data->hasFormat(QString::fromLatin1(MessageListModel::MimeType)))
        return false;
    // Only within the same account.
    const QList<Key> keys = MessageListModel::keysFromMime(data);
    return !keys.isEmpty() && keys.first().account == m_nodes[n].id.account;
}

bool FolderModel::dropMimeData(const QMimeData *data, Qt::DropAction action, int row, int column,
                               const QModelIndex &parent)
{
    if (!canDropMimeData(data, action, row, column, parent))
        return false;
    const FolderId target = idAt(parent);
    const QList<Key> keys = MessageListModel::keysFromMime(data);
    const auto ffiKeys = toFfi(keys);
    core().move_messages(slice(ffiKeys), rs(target.account), target.folder);
    // The core updates the lists itself; report "not moved" so the source
    // view doesn't try to remove rows.
    return false;
}

} // namespace tern
