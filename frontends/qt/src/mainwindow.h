// Three panes: folder tree → message list → message view (§10).
#pragma once

#include "core.h"
#include "foldermodel.h"

#include <QHash>
#include <QMainWindow>

class QLabel;
class QLineEdit;
class QProgressBar;
class QSettings;
class QSplitter;
class QTimer;
class QToolButton;
class QTreeView;

namespace tern {

class MessageListModel;
class MessageView;

class MainWindow : public QMainWindow {
    Q_OBJECT
public:
    explicit MainWindow(const QString &profile, QWidget *parent = nullptr);
    ~MainWindow() override;

    void raiseFromOtherInstance(const QString &activationToken);
    // A new-mail notification was clicked: show that message in its folder.
    void showNotifiedMessage(const Key &key, qint64 folder, const QString &activationToken);

protected:
    void closeEvent(QCloseEvent *event) override;

private:
    void createActions();
    void restoreState();
    void saveState();
    // Columns and sort indicator from the config; true if anything changed.
    bool applyListLayout();
    void saveListHeader();

    void refreshFolders();
    void folderSelected();
    void reopenList();
    // Per folder: scroll position and selected message, across restarts.
    void saveListPosition();
    void restoreListPosition();
    void listChanged(quint32 count);
    void messageActivated();
    void messageLoaded(const std::shared_ptr<ffi::MessageView> &view);
    void updateActions();

    QList<Key> selectedKeys() const;
    Key currentKey() const;
    // `row` is a core index (as from list_index_of), not a view row.
    void selectRow(int row, bool open);

    void compose();
    void reply(ffi::ReplyMode mode);
    void deleteSelected();
    void archiveSelected();
    void markRead(bool read);
    void toggleFlag();
    void viewSource();
    void openMailto(const QUrl &url);
    void openDraft(const ffi::Draft &draft);

    void accountStatus(const QString &account, AccountState state, const QString &text);
    void progress(const QString &account, const QString &folder, quint32 done, quint32 total);
    void configChanged(const QStringList &issues);
    void showConfigIssues();

    QString m_profile;
    QSettings *m_settings = nullptr;

    FolderModel *m_folderModel = nullptr;
    QTreeView *m_folders = nullptr;
    MessageListModel *m_listModel = nullptr;
    QTreeView *m_list = nullptr;
    MessageView *m_view = nullptr;
    QSplitter *m_hsplit = nullptr;
    QSplitter *m_vsplit = nullptr;
    QLineEdit *m_search = nullptr;
    QTimer *m_searchTimer = nullptr;

    QLabel *m_status = nullptr;
    QProgressBar *m_progress = nullptr;
    QToolButton *m_issuesButton = nullptr;
    QStringList m_issues;
    QHash<QString, QString> m_accountStatus;

    FolderId m_folder;
    bool m_threaded = true;
    // Quick filters (session only).
    ffi::ListFilter m_filter{false, false, false};
    bool m_initialSelectionDone = false;
    bool m_restoring = false;
    // Message to select once the list of its folder is open (notification).
    Key m_pendingShow;
    bool m_layoutApplied = false;
    ffi::ListColumn m_sortBy = ffi::ListColumn::Date;
    Qt::SortOrder m_sortOrder = Qt::DescendingOrder;

    QAction *m_replyAct = nullptr;
    QAction *m_replyAllAct = nullptr;
    QAction *m_forwardAct = nullptr;
    QAction *m_deleteAct = nullptr;
    QAction *m_archiveAct = nullptr;
    QAction *m_markReadAct = nullptr;
    QAction *m_markUnreadAct = nullptr;
    QAction *m_flagAct = nullptr;
    QAction *m_threadedAct = nullptr;
    QAction *m_sourceAct = nullptr;
};

} // namespace tern
