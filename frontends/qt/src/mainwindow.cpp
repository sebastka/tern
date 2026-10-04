#include "mainwindow.h"

#include "composewindow.h"
#include "messagelistmodel.h"
#include "messageview.h"

#include <QAction>
#include <QApplication>
#include <QCloseEvent>
#include <QDir>
#include <QHeaderView>
#include <QLabel>
#include <QLineEdit>
#include <QMenuBar>
#include <QMessageBox>
#include <QProgressBar>
#include <QSettings>
#include <QSplitter>
#include <QStandardPaths>
#include <QStatusBar>
#include <QTimer>
#include <QToolBar>
#include <QToolButton>
#include <QTreeView>
#include <QUrlQuery>
#include <QWindow>

namespace tern {

MainWindow::MainWindow(const QString &profile, QWidget *parent) : QMainWindow(parent), m_profile(profile)
{
    // The desktop appends the application name ("private — Tern").
    setWindowTitle(profile);

    // UI state is unimportant, persistent data: $XDG_STATE_HOME (§7). The
    // config directory is never written.
    const QString stateDir = QStandardPaths::writableLocation(QStandardPaths::GenericStateLocation)
                             + QStringLiteral("/tern");
    QDir().mkpath(stateDir);
    m_settings = new QSettings(stateDir + QStringLiteral("/qt-%1.ini").arg(profile), QSettings::IniFormat, this);

    m_folderModel = new FolderModel(this);
    m_folders = new QTreeView(this);
    m_folders->setModel(m_folderModel);
    m_folders->setHeaderHidden(true);
    m_folders->setUniformRowHeights(true);
    m_folders->setDragDropMode(QAbstractItemView::DropOnly);
    m_folders->setDropIndicatorShown(true);
    m_folders->setDefaultDropAction(Qt::MoveAction);
    connect(m_folders->selectionModel(), &QItemSelectionModel::currentChanged, this, &MainWindow::folderSelected);

    m_listModel = new MessageListModel(this);
    m_list = new QTreeView(this);
    m_list->setModel(m_listModel);
    m_list->setRootIsDecorated(false);
    m_list->setUniformRowHeights(true);
    m_list->setAllColumnsShowFocus(true);
    m_list->setSelectionMode(QAbstractItemView::ExtendedSelection);
    m_list->setDragEnabled(true);
    m_list->setDragDropMode(QAbstractItemView::DragOnly);
    m_list->setTextElideMode(Qt::ElideRight);
    m_list->header()->setStretchLastSection(false);
    m_list->header()->setSectionResizeMode(MessageListModel::ColFlag, QHeaderView::Fixed);
    m_list->header()->setSectionResizeMode(MessageListModel::ColAttachment, QHeaderView::Fixed);
    m_list->header()->setSectionResizeMode(MessageListModel::ColSubject, QHeaderView::Stretch);
    m_list->header()->resizeSection(MessageListModel::ColFlag, 28);
    m_list->header()->resizeSection(MessageListModel::ColAttachment, 28);
    m_list->header()->resizeSection(MessageListModel::ColCorrespondent, 220);
    m_list->header()->resizeSection(MessageListModel::ColDate, 130);
    connect(m_list->selectionModel(), &QItemSelectionModel::currentChanged, this, &MainWindow::messageActivated);
    connect(m_list->selectionModel(), &QItemSelectionModel::selectionChanged, this, &MainWindow::updateActions);

    m_view = new MessageView(this);
    connect(m_view, &MessageView::mailtoClicked, this, &MainWindow::openMailto);

    m_vsplit = new QSplitter(Qt::Vertical, this);
    m_vsplit->addWidget(m_list);
    m_vsplit->addWidget(m_view);
    m_vsplit->setStretchFactor(1, 2);
    m_hsplit = new QSplitter(Qt::Horizontal, this);
    m_hsplit->addWidget(m_folders);
    m_hsplit->addWidget(m_vsplit);
    m_hsplit->setStretchFactor(1, 4);
    setCentralWidget(m_hsplit);

    m_status = new QLabel(this);
    m_progress = new QProgressBar(this);
    m_progress->setMaximumWidth(220);
    m_progress->setTextVisible(true);
    m_progress->hide();
    m_issuesButton = new QToolButton(this);
    m_issuesButton->setIcon(QIcon::fromTheme(QStringLiteral("dialog-warning")));
    m_issuesButton->setText(tr("Configuration problems"));
    m_issuesButton->setToolButtonStyle(Qt::ToolButtonTextBesideIcon);
    m_issuesButton->setAutoRaise(true);
    m_issuesButton->hide();
    connect(m_issuesButton, &QToolButton::clicked, this, &MainWindow::showConfigIssues);
    statusBar()->addWidget(m_status, 1);
    statusBar()->addPermanentWidget(m_issuesButton);
    statusBar()->addPermanentWidget(m_progress);

    m_threaded = core().threaded_by_default();
    createActions();
    restoreState();

    EventBridge *b = bridge();
    connect(b, &EventBridge::folderTreeChanged, this, &MainWindow::refreshFolders);
    connect(b, &EventBridge::listChanged, this, &MainWindow::listChanged);
    connect(b, &EventBridge::messageLoaded, this, &MainWindow::messageLoaded);
    connect(b, &EventBridge::composeReady, this, [this](const std::shared_ptr<ffi::Draft> &d) { openDraft(*d); });
    connect(b, &EventBridge::accountStatus, this, &MainWindow::accountStatus);
    connect(b, &EventBridge::progress, this, &MainWindow::progress);
    connect(b, &EventBridge::configChanged, this, &MainWindow::configChanged);
    connect(b, &EventBridge::sendResult, this, [this](bool ok, const QString &text) {
        if (ok)
            statusBar()->showMessage(text, 5000);
        else
            QMessageBox::warning(this, tr("Sending Mail"), text);
    });
    connect(b, &EventBridge::error, this, [this](const QString &text) { statusBar()->showMessage(text, 10000); });
    connect(b, &EventBridge::raiseWindow, this, &MainWindow::raiseFromOtherInstance);

    configChanged(qsl(core().config_issues()));
    refreshFolders();
    updateActions();
}

MainWindow::~MainWindow() = default;

void MainWindow::createActions()
{
    auto action = [this](const QString &icon, const QString &text, const QKeySequence &key, auto slot) {
        auto *a = new QAction(QIcon::fromTheme(icon), text, this);
        if (!key.isEmpty())
            a->setShortcut(key);
        connect(a, &QAction::triggered, this, slot);
        return a;
    };
    QAction *sync = action(QStringLiteral("mail-receive"), tr("Get Mail"), QKeySequence(Qt::Key_F5),
                           [] { core().sync_now(); });
    QAction *newMsg = action(QStringLiteral("mail-message-new"), tr("New Message"), QKeySequence::New,
                             &MainWindow::compose);
    m_replyAct = action(QStringLiteral("mail-reply-sender"), tr("Reply"), QKeySequence(Qt::CTRL | Qt::Key_R),
                        [this] { reply(ffi::ReplyMode::Reply); });
    m_replyAllAct = action(QStringLiteral("mail-reply-all"), tr("Reply All"),
                           QKeySequence(Qt::CTRL | Qt::SHIFT | Qt::Key_R), [this] { reply(ffi::ReplyMode::ReplyAll); });
    m_forwardAct = action(QStringLiteral("mail-forward"), tr("Forward"), QKeySequence(Qt::CTRL | Qt::Key_L),
                          [this] { reply(ffi::ReplyMode::Forward); });
    m_archiveAct = action(QStringLiteral("mail-move"), tr("Archive"), QKeySequence(Qt::Key_A),
                          &MainWindow::archiveSelected);
    // Breeze has no mail-move icon.
    m_archiveAct->setIcon(QIcon::fromTheme(
        QStringLiteral("archive-insert"),
        QIcon::fromTheme(QStringLiteral("mail-archive"), QIcon::fromTheme(QStringLiteral("folder-download")))));
    m_deleteAct = action(QStringLiteral("edit-delete"), tr("Delete"), QKeySequence::Delete,
                         &MainWindow::deleteSelected);
    m_markReadAct = action(QStringLiteral("mail-mark-read"), tr("Mark as Read"), QKeySequence(Qt::Key_M),
                           [this] { markRead(true); });
    m_markUnreadAct = action(QStringLiteral("mail-mark-unread"), tr("Mark as Unread"),
                             QKeySequence(Qt::SHIFT | Qt::Key_M), [this] { markRead(false); });
    m_flagAct = action(QStringLiteral("flag"), tr("Toggle Flag"), QKeySequence(Qt::Key_S), &MainWindow::toggleFlag);
    m_threadedAct = action(QStringLiteral("view-list-tree"), tr("Threaded View"), QKeySequence(Qt::Key_T), [this] {
        m_threaded = m_threadedAct->isChecked();
        reopenList();
    });
    m_threadedAct->setCheckable(true);
    m_threadedAct->setChecked(m_threaded);
    // Single-key shortcuts only while the message list has focus, so typing
    // elsewhere (search) isn't hijacked.
    for (QAction *a : {m_archiveAct, m_deleteAct, m_markReadAct, m_markUnreadAct, m_flagAct, m_threadedAct}) {
        a->setShortcutContext(Qt::WidgetWithChildrenShortcut);
        m_list->addAction(a);
    }

    QToolBar *tb = addToolBar(tr("Main"));
    tb->setObjectName(QStringLiteral("mainToolBar"));
    tb->setToolButtonStyle(Qt::ToolButtonTextBesideIcon);
    tb->addAction(sync);
    tb->addAction(newMsg);
    tb->addSeparator();
    tb->addAction(m_replyAct);
    tb->addAction(m_replyAllAct);
    tb->addAction(m_forwardAct);
    tb->addSeparator();
    tb->addAction(m_archiveAct);
    tb->addAction(m_deleteAct);
    tb->addAction(m_flagAct);

    m_search = new QLineEdit(this);
    m_search->setPlaceholderText(tr("Search subject, sender, recipients…"));
    m_search->setClearButtonEnabled(true);
    m_search->setMaximumWidth(320);
    m_searchTimer = new QTimer(this);
    m_searchTimer->setSingleShot(true);
    m_searchTimer->setInterval(300);
    connect(m_search, &QLineEdit::textChanged, m_searchTimer, qOverload<>(&QTimer::start));
    connect(m_searchTimer, &QTimer::timeout, this, &MainWindow::reopenList);
    auto *spacer = new QWidget(this);
    spacer->setSizePolicy(QSizePolicy::Expanding, QSizePolicy::Preferred);
    tb->addWidget(spacer);
    tb->addWidget(m_search);
    auto *focusSearch = new QAction(this);
    focusSearch->setShortcut(QKeySequence::Find);
    connect(focusSearch, &QAction::triggered, this, [this] {
        m_search->setFocus();
        m_search->selectAll();
    });
    addAction(focusSearch);

    QMenu *file = menuBar()->addMenu(tr("&File"));
    file->addAction(newMsg);
    file->addAction(sync);
    file->addSeparator();
    file->addAction(QIcon::fromTheme(QStringLiteral("application-exit")), tr("&Quit"), QKeySequence::Quit, this,
                    &QWidget::close);
    QMenu *view = menuBar()->addMenu(tr("&View"));
    view->addAction(m_threadedAct);
    QMenu *msg = menuBar()->addMenu(tr("&Message"));
    for (QAction *a : {m_replyAct, m_replyAllAct, m_forwardAct})
        msg->addAction(a);
    msg->addSeparator();
    for (QAction *a : {m_markReadAct, m_markUnreadAct, m_flagAct})
        msg->addAction(a);
    msg->addSeparator();
    msg->addAction(m_archiveAct);
    msg->addAction(m_deleteAct);
    QMenu *help = menuBar()->addMenu(tr("&Help"));
    help->addAction(tr("Configuration Problems…"), this, &MainWindow::showConfigIssues);
    help->addAction(tr("About Tern"), this, [this] {
        QMessageBox::about(this, tr("About Tern"),
                           tr("<b>Tern</b> %1<br>A mail client that speaks IMAP and SMTP.<br>"
                              "Configuration: <tt>$XDG_CONFIG_HOME/tern/</tt>")
                               .arg(QStringLiteral(TERN_VERSION)));
    });

    m_list->setContextMenuPolicy(Qt::ActionsContextMenu);
    for (QAction *a : {m_replyAct, m_replyAllAct, m_forwardAct})
        m_list->addAction(a);
}

void MainWindow::restoreState()
{
    restoreGeometry(m_settings->value(QStringLiteral("geometry")).toByteArray());
    QMainWindow::restoreState(m_settings->value(QStringLiteral("windowState")).toByteArray());
    m_hsplit->restoreState(m_settings->value(QStringLiteral("hsplit")).toByteArray());
    m_vsplit->restoreState(m_settings->value(QStringLiteral("vsplit")).toByteArray());
    m_list->header()->restoreState(m_settings->value(QStringLiteral("listHeader")).toByteArray());
    if (m_settings->contains(QStringLiteral("threaded"))) {
        m_threaded = m_settings->value(QStringLiteral("threaded")).toBool();
        m_threadedAct->setChecked(m_threaded);
    }
    if (!m_settings->contains(QStringLiteral("geometry")))
        resize(1280, 820);
}

void MainWindow::saveState()
{
    m_settings->setValue(QStringLiteral("geometry"), saveGeometry());
    m_settings->setValue(QStringLiteral("windowState"), QMainWindow::saveState());
    m_settings->setValue(QStringLiteral("hsplit"), m_hsplit->saveState());
    m_settings->setValue(QStringLiteral("vsplit"), m_vsplit->saveState());
    m_settings->setValue(QStringLiteral("listHeader"), m_list->header()->saveState());
    m_settings->setValue(QStringLiteral("threaded"), m_threaded);
    m_settings->setValue(QStringLiteral("lastFolderAccount"), m_folder.account);
    m_settings->setValue(QStringLiteral("lastFolder"), m_folder.folder);
}

void MainWindow::closeEvent(QCloseEvent *event)
{
    saveState();
    event->accept();
}

void MainWindow::raiseFromOtherInstance(const QString &activationToken)
{
    // On Wayland, focus may only be taken with a token from the launcher.
    if (!activationToken.isEmpty())
        qputenv("XDG_ACTIVATION_TOKEN", activationToken.toUtf8());
    setWindowState((windowState() & ~Qt::WindowMinimized) | Qt::WindowActive);
    show();
    raise();
    activateWindow();
    if (QWindow *w = windowHandle())
        w->requestActivate();
}

// ---------------------------------------------------------------- folders

void MainWindow::refreshFolders()
{
    const FolderId current = m_folder;
    m_folderModel->refresh();
    m_folders->expandAll();
    if (!m_initialSelectionDone) {
        const FolderId last{m_settings->value(QStringLiteral("lastFolderAccount")).toString(),
                            m_settings->value(QStringLiteral("lastFolder")).toLongLong()};
        QModelIndex idx = last.account.isEmpty() ? QModelIndex() : m_folderModel->indexOf(last);
        if (!idx.isValid())
            idx = m_folderModel->firstInbox();
        if (idx.isValid()) {
            m_initialSelectionDone = true;
            m_folders->setCurrentIndex(idx);
        }
        return;
    }
    const QModelIndex idx = m_folderModel->indexOf(current);
    if (idx.isValid() && m_folders->currentIndex() != idx) {
        QSignalBlocker block(m_folders->selectionModel());
        m_folders->setCurrentIndex(idx);
    } else if (!idx.isValid() && !current.account.isEmpty()) {
        // The folder disappeared (deleted on the server or config change).
        m_folder = {};
        reopenList();
    }
}

void MainWindow::folderSelected()
{
    const QModelIndex idx = m_folders->currentIndex();
    if (!idx.data(FolderModel::SelectableRole).toBool())
        return;
    const FolderId id = m_folderModel->idAt(idx);
    if (id == m_folder)
        return;
    m_folder = id;
    m_search->blockSignals(true);
    m_search->clear();
    m_search->blockSignals(false);
    reopenList();
    m_view->clear();
    updateActions();
}

void MainWindow::reopenList()
{
    const QString role = m_folderModel->data(m_folderModel->indexOf(m_folder), FolderModel::FolderRoleName).toString();
    const bool outgoing = role == u"sent" || role == u"drafts";
    m_listModel->open(m_folder.account, m_folder.folder, m_threaded, m_search->text(), outgoing);
    const Key shown = m_view->currentKey();
    if (shown.valid()) {
        const qint64 row = core().list_index_of(shown.toFfi());
        if (row >= 0)
            selectRow(static_cast<int>(row), false);
    }
    updateActions();
}

// --------------------------------------------------------------- messages

void MainWindow::listChanged(quint32 count)
{
    // Keep the current message selected across the reset; if it's gone
    // (moved or deleted), select the message that took its place.
    const Key current = currentKey();
    const int oldRow = m_list->currentIndex().row();
    const QList<Key> selected = selectedKeys();
    m_listModel->reload(count);
    if (!current.valid())
        return;
    const qint64 row = core().list_index_of(current.toFfi());
    if (row >= 0) {
        selectRow(static_cast<int>(row), false);
        for (const Key &k : selected) {
            const qint64 r = core().list_index_of(k.toFfi());
            if (r >= 0 && k != current)
                m_list->selectionModel()->select(m_listModel->index(static_cast<int>(r), 0),
                                                 QItemSelectionModel::Select | QItemSelectionModel::Rows);
        }
    } else if (count > 0 && oldRow >= 0) {
        selectRow(qMin(oldRow, static_cast<int>(count) - 1), true);
    } else {
        m_view->clear();
    }
    updateActions();
}

void MainWindow::selectRow(int row, bool open)
{
    const QModelIndex idx = m_listModel->index(row, MessageListModel::ColSubject);
    m_restoring = !open;
    m_list->selectionModel()->setCurrentIndex(idx, QItemSelectionModel::ClearAndSelect | QItemSelectionModel::Rows);
    m_restoring = false;
    m_list->scrollTo(idx);
}

void MainWindow::messageActivated()
{
    updateActions();
    if (m_restoring)
        return;
    const Key key = currentKey();
    if (!key.valid() || key == m_view->currentKey())
        return;
    core().open_message(key.toFfi(), false);
    const Row *r = m_listModel->rowAt(m_list->currentIndex().row());
    if (r && r->unread) {
        const auto keys = toFfi({key});
        core().mark_read(slice(keys), true);
    }
}

void MainWindow::messageLoaded(const std::shared_ptr<ffi::MessageView> &view)
{
    // Ignore late results for messages no longer selected.
    if (Key::from(view->key) != currentKey())
        return;
    m_view->showMessage(view);
}

QList<Key> MainWindow::selectedKeys() const
{
    QList<Key> keys;
    for (const QModelIndex &i : m_list->selectionModel()->selectedRows()) {
        const Key k = m_listModel->keyAt(i.row());
        if (k.valid())
            keys << k;
    }
    return keys;
}

Key MainWindow::currentKey() const
{
    const QModelIndex idx = m_list->currentIndex();
    return idx.isValid() ? m_listModel->keyAt(idx.row()) : Key{};
}

void MainWindow::updateActions()
{
    const bool one = currentKey().valid();
    const bool any = !m_list->selectionModel()->selectedRows().isEmpty();
    for (QAction *a : {m_replyAct, m_replyAllAct, m_forwardAct})
        a->setEnabled(one);
    for (QAction *a : {m_deleteAct, m_markReadAct, m_markUnreadAct, m_flagAct})
        a->setEnabled(any);
    // Archiving needs an `archive` folder pattern for the account.
    bool canArchive = false;
    for (const auto &a : core().accounts())
        if (qs(a.id) == m_folder.account)
            canArchive = a.can_archive;
    m_archiveAct->setEnabled(any && canArchive);
    m_archiveAct->setToolTip(canArchive || m_folder.account.isEmpty()
                                 ? tr("Archive (A)")
                                 : tr("No archive folder configured for this account.\n"
                                      "Add for example archive = \"Archive/{year}\" to its account file."));
}

// ---------------------------------------------------------------- actions

void MainWindow::compose()
{
    QString account = m_folder.account;
    if (account.isEmpty()) {
        const auto accounts = core().accounts();
        if (accounts.empty()) {
            QMessageBox::information(this, tr("New Message"), tr("No account is configured."));
            return;
        }
        account = qs(accounts[0].id);
    }
    openDraft(core().new_draft(rs(account)));
}

void MainWindow::openDraft(const ffi::Draft &draft)
{
    auto *w = new ComposeWindow(draft);
    w->show();
}

void MainWindow::openMailto(const QUrl &url)
{
    const QString account = m_folder.account;
    if (account.isEmpty())
        return;
    ffi::Draft d = core().new_draft(rs(account));
    const QUrlQuery q(url);
    d.to = rs(url.path(QUrl::FullyDecoded));
    d.cc = rs(q.queryItemValue(QStringLiteral("cc"), QUrl::FullyDecoded));
    d.subject = rs(q.queryItemValue(QStringLiteral("subject"), QUrl::FullyDecoded));
    d.body = rs(q.queryItemValue(QStringLiteral("body"), QUrl::FullyDecoded));
    openDraft(d);
}

void MainWindow::reply(ffi::ReplyMode mode)
{
    const Key key = currentKey();
    if (key.valid())
        core().prepare_reply(key.toFfi(), mode);
}

void MainWindow::deleteSelected()
{
    const auto keys = toFfi(selectedKeys());
    if (!keys.empty())
        core().delete_messages(slice(keys));
}

void MainWindow::archiveSelected()
{
    const auto keys = toFfi(selectedKeys());
    if (!keys.empty())
        core().archive_messages(slice(keys));
}

void MainWindow::markRead(bool read)
{
    const auto keys = toFfi(selectedKeys());
    if (!keys.empty())
        core().mark_read(slice(keys), read);
}

void MainWindow::toggleFlag()
{
    const QList<Key> keys = selectedKeys();
    if (keys.isEmpty())
        return;
    // Flag all unless all are flagged already.
    bool allFlagged = true;
    for (const QModelIndex &i : m_list->selectionModel()->selectedRows())
        if (const Row *r = m_listModel->rowAt(i.row()); r && !r->flagged)
            allFlagged = false;
    const auto ffiKeys = toFfi(keys);
    core().mark_flagged(slice(ffiKeys), !allFlagged);
}

// ----------------------------------------------------------------- status

void MainWindow::accountStatus(const QString &account, AccountState state, const QString &text)
{
    QString name = account;
    for (const auto &a : core().accounts())
        if (qs(a.id) == account)
            name = qs(a.name);
    QString s;
    switch (state) {
    case AccountState::Connecting:
        s = tr("%1: connecting…").arg(name);
        break;
    case AccountState::Syncing:
        s = tr("%1: syncing…").arg(name);
        break;
    case AccountState::Online:
        s = QString();
        break;
    case AccountState::Offline:
        s = tr("%1: offline (%2)").arg(name, text);
        break;
    case AccountState::Error:
        s = tr("%1: %2").arg(name, text);
        break;
    }
    if (s.isEmpty())
        m_accountStatus.remove(account);
    else
        m_accountStatus.insert(account, s);
    QStringList all = m_accountStatus.values();
    all.sort();
    m_status->setText(all.isEmpty() ? tr("Up to date") : all.join(QStringLiteral(" · ")));
}

void MainWindow::progress(const QString &account, const QString &folder, quint32 done, quint32 total)
{
    Q_UNUSED(account);
    if (total == 0 || done >= total) {
        m_progress->hide();
        return;
    }
    m_progress->setRange(0, static_cast<int>(total));
    m_progress->setValue(static_cast<int>(done));
    m_progress->setFormat(QStringLiteral("%1 %p%").arg(folder));
    m_progress->setToolTip(tr("%1: %2 of %3").arg(folder).arg(done).arg(total));
    m_progress->show();
}

void MainWindow::configChanged(const QStringList &issues)
{
    updateActions();
    m_issues = issues;
    m_issuesButton->setVisible(!issues.isEmpty());
    if (!issues.isEmpty())
        statusBar()->showMessage(tr("The configuration has errors; the last valid configuration stays active."),
                                 10000);
}

void MainWindow::showConfigIssues()
{
    if (m_issues.isEmpty()) {
        QMessageBox::information(this, tr("Configuration"), tr("The configuration has no problems."));
        return;
    }
    QMessageBox box(QMessageBox::Warning, tr("Configuration Problems"),
                    tr("Fix these problems in the configuration files. Changes are picked up automatically."),
                    QMessageBox::Close, this);
    box.setDetailedText(m_issues.join(u'\n'));
    box.exec();
}

} // namespace tern
