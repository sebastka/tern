// Windowed message list (§4): rows are read from the core in pages, on
// demand, so folders with 100k messages cost only what is visible.
#pragma once

#include "core.h"

#include <QAbstractTableModel>
#include <QHash>
#include <QList>

namespace tern {

struct Row {
    Key key;
    quint32 depth = 0;
    quint32 threadSize = 0;
    qint64 date = 0;
    QString from;
    QString to;
    QString subject;
    bool unread = false;
    bool flagged = false;
    bool answered = false;
    bool forwarded = false;
    bool hasAttachments = false;
    bool encrypted = false;
    quint32 size = 0;
};

class MessageListModel : public QAbstractTableModel {
    Q_OBJECT
public:
    using Column = ffi::ListColumn;
    static constexpr auto MimeType = "application/x-tern-message-keys";

    explicit MessageListModel(QObject *parent = nullptr);

    // Columns from `[ui.message_list]`, left to right.
    void setColumns(const QList<Column> &columns);
    QList<Column> columns() const { return m_columns; }
    Column columnAt(int section) const { return m_columns.value(section, Column::Subject); }
    // Section showing `column`, or -1.
    int sectionOf(Column column) const { return static_cast<int>(m_columns.indexOf(column)); }

    // Switch to a folder (empty account = no list).
    void open(const QString &account, qint64 folder, bool threaded, const QString &query, bool showRecipients);
    // The core reports the list changed: reset, keeping no stale rows.
    void reload(quint32 count);

    const Row *rowAt(int row) const;
    Key keyAt(int row) const;

    int rowCount(const QModelIndex &parent = {}) const override;
    int columnCount(const QModelIndex &parent = {}) const override;
    QVariant data(const QModelIndex &index, int role) const override;
    QVariant headerData(int section, Qt::Orientation orientation, int role) const override;
    Qt::ItemFlags flags(const QModelIndex &index) const override;
    QStringList mimeTypes() const override;
    QMimeData *mimeData(const QModelIndexList &indexes) const override;
    Qt::DropActions supportedDragActions() const override;

    static QList<Key> keysFromMime(const QMimeData *data);

private:
    static constexpr int PageSize = 128;
    static constexpr int MaxPages = 32;

    quint32 m_count = 0;
    bool m_showRecipients = false;
    QList<Column> m_columns{Column::Flag, Column::Subject, Column::Correspondent, Column::Date, Column::Attachment};
    // page index → rows. Mutable: filled lazily from const data().
    mutable QHash<int, QList<Row>> m_pages;
    mutable QList<int> m_pageOrder; // LRU, most recent last
};

} // namespace tern
