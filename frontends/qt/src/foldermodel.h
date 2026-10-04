// Folder tree: one root per account, real IMAP hierarchy below (§10).
#pragma once

#include "core.h"

#include <QAbstractItemModel>
#include <QList>

namespace tern {

struct FolderId {
    QString account;
    qint64 folder = 0; // 0 = account root
    bool operator==(const FolderId &) const = default;
};

class FolderModel : public QAbstractItemModel {
    Q_OBJECT
public:
    enum Roles { AccountRole = Qt::UserRole + 1, FolderRole, SelectableRole, FolderRoleName, UnreadRole };

    explicit FolderModel(QObject *parent = nullptr);

    // Re-read the tree from the core. Keeps persistent indexes (selection)
    // when only counts changed; resets otherwise.
    void refresh();

    FolderId idAt(const QModelIndex &index) const;
    QModelIndex indexOf(const FolderId &id) const;
    // First INBOX, for the initial selection.
    QModelIndex firstInbox() const;

    QModelIndex index(int row, int column, const QModelIndex &parent = {}) const override;
    QModelIndex parent(const QModelIndex &child) const override;
    int rowCount(const QModelIndex &parent = {}) const override;
    int columnCount(const QModelIndex &parent = {}) const override;
    QVariant data(const QModelIndex &index, int role) const override;
    Qt::ItemFlags flags(const QModelIndex &index) const override;
    QStringList mimeTypes() const override;
    bool canDropMimeData(const QMimeData *data, Qt::DropAction action, int row, int column,
                         const QModelIndex &parent) const override;
    bool dropMimeData(const QMimeData *data, Qt::DropAction action, int row, int column,
                      const QModelIndex &parent) override;
    Qt::DropActions supportedDropActions() const override;

private:
    struct Node {
        FolderId id;
        int parent = -1;
        QString name;
        QString path;
        QString role;
        bool selectable = false;
        quint32 unread = 0;
        quint32 total = 0;
        QList<int> children; // indexes into m_nodes
        int row = 0;         // position among siblings
    };

    QList<Node> m_nodes;
    QList<int> m_roots;

    int nodeOf(const QModelIndex &index) const;
};

} // namespace tern
