#include "messageview.h"

#include "sourcewindow.h"

#include <QBuffer>
#include <QCheckBox>
#include <QDBusConnection>
#include <QDBusUnixFileDescriptor>
#include <QDir>
#include <QDBusMessage>
#include <QDBusPendingCallWatcher>
#include <QDateTime>
#include <QDesktopServices>
#include <QFile>
#include <QFileDialog>
#include <QFileInfo>
#include <QGuiApplication>
#include <QClipboard>
#include <QCoreApplication>
#include <QHBoxLayout>
#include <QLabel>
#include <QLocale>
#include <QMenu>
#include <QMessageBox>
#include <QPushButton>
#include <QScrollArea>
#include <QStandardPaths>
#include <QTextBrowser>
#include <QToolButton>
#include <QVBoxLayout>
#include <QWebEngineContextMenuRequest>
#include <QWebEngineProfile>
#include <QWebEngineSettings>
#include <QWebEngineUrlRequestJob>
#include <QWebEngineUrlScheme>
#include <QWebEngineView>

namespace tern {

void registerUrlScheme()
{
    QWebEngineUrlScheme scheme(Scheme);
    scheme.setSyntax(QWebEngineUrlScheme::Syntax::Path);
    // Not a "local" scheme: local pages could never load remote content, and
    // we want to allow that per message. The CSP in each document and the
    // interceptor below do the blocking.
    scheme.setFlags({});
    QWebEngineUrlScheme::registerScheme(scheme);
}

void openExternally(const QUrl &url)
{
    // org.freedesktop.portal.OpenURI works on every desktop with
    // xdg-desktop-portal (and inside sandboxes); fall back otherwise.
    QDBusMessage call = QDBusMessage::createMethodCall(
        QStringLiteral("org.freedesktop.portal.Desktop"), QStringLiteral("/org/freedesktop/portal/desktop"),
        QStringLiteral("org.freedesktop.portal.OpenURI"), QStringLiteral("OpenURI"));
    call << QString() << url.toString() << QVariantMap{};
    auto *watcher = new QDBusPendingCallWatcher(QDBusConnection::sessionBus().asyncCall(call, 5000));
    QObject::connect(watcher, &QDBusPendingCallWatcher::finished, watcher, [url](QDBusPendingCallWatcher *w) {
        if (w->isError())
            QDesktopServices::openUrl(url);
        w->deleteLater();
    });
}

QString openedAttachmentsDir()
{
    // $XDG_RUNTIME_DIR: per user, mode 0700, in memory (tmpfs), gone at logout.
    // Per process: another Tern (other profile) cleans up only its own.
    return QStandardPaths::writableLocation(QStandardPaths::RuntimeLocation)
           + QStringLiteral("/tern/opened-%1").arg(QCoreApplication::applicationPid());
}

void openFileExternally(const QString &path)
{
    QFile file(path);
    if (!file.open(QIODevice::ReadOnly)) {
        QDesktopServices::openUrl(QUrl::fromLocalFile(path));
        return;
    }
    QDBusMessage call = QDBusMessage::createMethodCall(
        QStringLiteral("org.freedesktop.portal.Desktop"), QStringLiteral("/org/freedesktop/portal/desktop"),
        QStringLiteral("org.freedesktop.portal.OpenURI"), QStringLiteral("OpenFile"));
    call << QString() << QVariant::fromValue(QDBusUnixFileDescriptor(file.handle())) << QVariantMap{};
    auto *watcher = new QDBusPendingCallWatcher(QDBusConnection::sessionBus().asyncCall(call, 5000));
    QObject::connect(watcher, &QDBusPendingCallWatcher::finished, watcher, [path](QDBusPendingCallWatcher *w) {
        if (w->isError())
            QDesktopServices::openUrl(QUrl::fromLocalFile(path));
        w->deleteLater();
    });
}

// ---------------------------------------------------------------- scheme

void SchemeHandler::requestStarted(QWebEngineUrlRequestJob *job)
{
    const auto res = core().resource(rs(job->requestUrl().toString()));
    if (!res.found) {
        job->fail(QWebEngineUrlRequestJob::UrlNotFound);
        return;
    }
    auto *buffer = new QBuffer(job);
    buffer->setData(QByteArray(reinterpret_cast<const char *>(res.data.data()), static_cast<qsizetype>(res.data.size())));
    buffer->open(QIODevice::ReadOnly);
    job->reply(qs(res.name).toUtf8(), buffer);
}

void RequestInterceptor::interceptRequest(QWebEngineUrlRequestInfo &info)
{
    const QString scheme = info.requestUrl().scheme();
    if (scheme == QLatin1String(Scheme) || scheme == u"data")
        return;
    const bool remote = scheme == u"http" || scheme == u"https";
    // Main-frame navigations are handled (and refused) by the page.
    if (remote && m_allowRemote && info.resourceType() != QWebEngineUrlRequestInfo::ResourceTypeMainFrame)
        return;
    info.block(true);
}

bool MessagePage::acceptNavigationRequest(const QUrl &url, NavigationType type, bool isMainFrame)
{
    if (type == NavigationTypeLinkClicked) {
        if (url.scheme() == u"mailto")
            Q_EMIT mailtoClicked(url);
        else if (url.scheme() == u"http" || url.scheme() == u"https")
            openExternally(url);
        return false;
    }
    // Only our own documents (and our own setHtml() placeholders) load.
    if (!isMainFrame || type == NavigationTypeFormSubmitted)
        return false;
    return url.scheme() == QLatin1String(Scheme) || (url.scheme() == u"data" && type == NavigationTypeTyped);
}

QWebEnginePage *MessagePage::createWindow(WebWindowType) { return nullptr; }

// ------------------------------------------------------------------ view

MessageView::MessageView(QWidget *parent) : QWidget(parent)
{
    // No storage name → off-the-record: no cookies, cache or storage on disk.
    m_profile = new QWebEngineProfile(this);
    m_profile->setHttpCacheType(QWebEngineProfile::MemoryHttpCache);
    m_profile->setPersistentCookiesPolicy(QWebEngineProfile::NoPersistentCookies);
    m_profile->setSpellCheckEnabled(false);
    m_profile->installUrlSchemeHandler(Scheme, new SchemeHandler(this));
    m_interceptor = new RequestInterceptor(this);
    m_profile->setUrlRequestInterceptor(m_interceptor);

    QWebEngineSettings *s = m_profile->settings();
    s->setAttribute(QWebEngineSettings::JavascriptEnabled, false);
    s->setAttribute(QWebEngineSettings::JavascriptCanOpenWindows, false);
    s->setAttribute(QWebEngineSettings::JavascriptCanAccessClipboard, false);
    s->setAttribute(QWebEngineSettings::LocalContentCanAccessRemoteUrls, false);
    s->setAttribute(QWebEngineSettings::LocalContentCanAccessFileUrls, false);
    s->setAttribute(QWebEngineSettings::PluginsEnabled, false);
    s->setAttribute(QWebEngineSettings::PdfViewerEnabled, false);
    s->setAttribute(QWebEngineSettings::AutoLoadIconsForPage, false);
    s->setAttribute(QWebEngineSettings::DnsPrefetchEnabled, false);
    s->setAttribute(QWebEngineSettings::NavigateOnDropEnabled, false);
    s->setAttribute(QWebEngineSettings::ScreenCaptureEnabled, false);
    s->setAttribute(QWebEngineSettings::WebGLEnabled, false);
    s->setAttribute(QWebEngineSettings::WebRTCPublicInterfacesOnly, true);
    s->setAttribute(QWebEngineSettings::ErrorPageEnabled, false);

    m_page = new MessagePage(m_profile, this);
    connect(m_page, &MessagePage::mailtoClicked, this, &MessageView::mailtoClicked);
    m_web = new QWebEngineView(this);
    m_web->setPage(m_page);
    m_web->setContextMenuPolicy(Qt::CustomContextMenu);
    connect(m_web, &QWidget::customContextMenuRequested, this, &MessageView::showContextMenu);

    // Header.
    m_header = new QWidget(this);
    auto *hl = new QVBoxLayout(m_header);
    hl->setContentsMargins(8, 6, 8, 6);
    auto *top = new QHBoxLayout;
    m_subject = new QLabel(m_header);
    QFont f = m_subject->font();
    f.setBold(true);
    f.setPointSizeF(f.pointSizeF() * 1.2);
    m_subject->setFont(f);
    m_subject->setWordWrap(true);
    m_subject->setTextInteractionFlags(Qt::TextSelectableByMouse);
    m_plain = new QCheckBox(tr("Plain text"), m_header);
    connect(m_plain, &QCheckBox::toggled, this, &MessageView::load);
    m_headersButton = new QToolButton(m_header);
    m_headersButton->setText(tr("Headers"));
    m_headersButton->setIcon(QIcon::fromTheme(QStringLiteral("view-list-details")));
    m_headersButton->setToolButtonStyle(Qt::ToolButtonTextBesideIcon);
    m_headersButton->setAutoRaise(true);
    m_headersButton->setCheckable(true);
    m_headersButton->setToolTip(tr("Show all header fields"));
    connect(m_headersButton, &QToolButton::toggled, this, &MessageView::updateHeaderView);
    m_sourceButton = new QToolButton(m_header);
    m_sourceButton->setText(tr("Source"));
    m_sourceButton->setIcon(
        QIcon::fromTheme(QStringLiteral("view-source"), QIcon::fromTheme(QStringLiteral("text-x-generic"))));
    m_sourceButton->setToolButtonStyle(Qt::ToolButtonTextBesideIcon);
    m_sourceButton->setAutoRaise(true);
    m_sourceButton->setToolTip(tr("View the message as received (Ctrl+U)"));
    connect(m_sourceButton, &QToolButton::clicked, this, &MessageView::viewSource);
    top->addWidget(m_subject, 1);
    top->addWidget(m_headersButton, 0, Qt::AlignTop);
    top->addWidget(m_sourceButton, 0, Qt::AlignTop);
    top->addWidget(m_plain, 0, Qt::AlignTop);
    hl->addLayout(top);
    m_meta = new QLabel(m_header);
    m_meta->setTextFormat(Qt::RichText);
    m_meta->setWordWrap(true);
    m_meta->setTextInteractionFlags(Qt::TextSelectableByMouse);
    hl->addWidget(m_meta);
    // Plain rich text only: no links are followed, no resources loaded.
    m_allHeaders = new QTextBrowser(m_header);
    m_allHeaders->setOpenLinks(false);
    m_allHeaders->setOpenExternalLinks(false);
    m_allHeaders->setWordWrapMode(QTextOption::WrapAtWordBoundaryOrAnywhere);
    m_allHeaders->setMaximumHeight(240);
    m_allHeaders->hide();
    hl->addWidget(m_allHeaders);
    m_security = new QLabel(m_header);
    m_security->setWordWrap(true);
    m_security->setMargin(4);
    m_security->setAutoFillBackground(true);
    hl->addWidget(m_security);

    m_remoteBar = new QWidget(m_header);
    auto *rl = new QHBoxLayout(m_remoteBar);
    rl->setContentsMargins(0, 0, 0, 0);
    rl->addWidget(new QLabel(tr("Remote content was blocked to protect your privacy."), m_remoteBar), 1);
    auto *load = new QPushButton(tr("Load remote content"), m_remoteBar);
    connect(load, &QPushButton::clicked, this, [this] {
        if (m_key.valid())
            core().open_message(m_key.toFfi(), true);
    });
    rl->addWidget(load);
    hl->addWidget(m_remoteBar);

    m_attachmentBar = new QWidget(m_header);
    m_attachmentLayout = new QHBoxLayout(m_attachmentBar);
    m_attachmentLayout->setContentsMargins(0, 0, 0, 0);
    hl->addWidget(m_attachmentBar);

    auto *layout = new QVBoxLayout(this);
    layout->setContentsMargins(0, 0, 0, 0);
    layout->setSpacing(0);
    layout->addWidget(m_header);
    layout->addWidget(m_web, 1);
    clear();
}

MessageView::~MessageView()
{
    // Children are deleted in creation order, which would destroy the
    // profile before the page using it. Pages must go first.
    delete m_web;
    delete m_page;
}

void MessageView::clear()
{
    m_key = {};
    m_view.reset();
    m_header->hide();
    m_interceptor->setAllowRemote(false);
    m_web->setHtml(QString());
}

QString MessageView::currentText() const { return m_view ? qs(m_view->text) : QString(); }

bool MessageView::showsAllHeaders() const { return m_headersButton->isChecked(); }

void MessageView::setShowAllHeaders(bool on) { m_headersButton->setChecked(on); }

void MessageView::updateHeaderView()
{
    // The full list arrives with the body; until then the short form stays.
    const bool available = m_view && !m_view->headers.empty();
    const bool all = m_headersButton->isChecked() && available;
    m_allHeaders->setVisible(all);
    m_meta->setVisible(!all);
    m_headersButton->setEnabled(available);
    m_headersButton->setToolTip(available ? tr("Show all header fields")
                                          : tr("The header fields are shown once the message is downloaded"));
}

void MessageView::viewSource()
{
    if (m_view)
        showMessageSource(this, m_key, qs(m_view->subject));
}

static QString escaped(const QString &s) { return s.toHtmlEscaped(); }

void MessageView::showMessage(const std::shared_ptr<ffi::MessageView> &view)
{
    const Key key = Key::from(view->key);
    const bool sameMessage = key == m_key;
    m_key = key;
    m_view = view;
    m_header->show();

    m_subject->setText(qs(view->subject).isEmpty() ? tr("(no subject)") : qs(view->subject));
    QString meta = QStringLiteral("<b>%1</b> %2").arg(tr("From:"), escaped(qs(view->from)));
    if (!view->to.empty())
        meta += QStringLiteral("<br><b>%1</b> %2").arg(tr("To:"), escaped(qs(view->to)));
    if (!view->cc.empty())
        meta += QStringLiteral("<br><b>%1</b> %2").arg(tr("Cc:"), escaped(qs(view->cc)));
    meta += QStringLiteral("<br><b>%1</b> %2")
                .arg(tr("Date:"),
                     QLocale().toString(QDateTime::fromSecsSinceEpoch(view->date).toLocalTime(), QLocale::LongFormat));
    m_meta->setText(meta);

    QString all = QStringLiteral("<table cellspacing='0' cellpadding='1'>");
    for (const auto &h : view->headers)
        all += QStringLiteral("<tr><td valign='top' style='white-space:pre'><b>%1:</b> </td><td>%2</td></tr>")
                   .arg(escaped(qs(h.name)), escaped(qs(h.value)));
    all += QStringLiteral("</table>");
    m_allHeaders->setHtml(all);
    updateHeaderView();

    // Security banner: encryption and signature state, as decided in Rust.
    QString sec;
    QColor bg;
    if (view->decryption_failed) {
        sec = tr("This message is encrypted and could not be decrypted.");
        bg = QColor(0xf8, 0xd7, 0xda);
    } else {
        QStringList parts;
        if (view->encrypted)
            parts << tr("Encrypted");
        switch (view->signature) {
        case ffi::SignatureState::Good:
            bg = QColor(0xd4, 0xed, 0xda);
            break;
        case ffi::SignatureState::Warning:
        case ffi::SignatureState::Unknown:
            bg = QColor(0xff, 0xf3, 0xcd);
            break;
        case ffi::SignatureState::Bad:
            bg = QColor(0xf8, 0xd7, 0xda);
            break;
        default:
            if (view->encrypted)
                bg = QColor(0xd1, 0xec, 0xf1);
            break;
        }
        if (!view->signature_text.empty())
            parts << qs(view->signature_text);
        sec = parts.join(QStringLiteral(" — "));
    }
    m_security->setVisible(!sec.isEmpty());
    if (!sec.isEmpty()) {
        m_security->setText(sec);
        QPalette pal = m_security->palette();
        pal.setColor(QPalette::Window, bg);
        pal.setColor(QPalette::WindowText, Qt::black);
        m_security->setPalette(pal);
    }

    m_remoteBar->setVisible(view->has_remote_content && !view->remote_allowed);

    // Attachments.
    while (QLayoutItem *item = m_attachmentLayout->takeAt(0)) {
        delete item->widget();
        delete item;
    }
    for (const auto &a : view->attachments) {
        auto *b = new QToolButton(m_attachmentBar);
        const QString name = qs(a.filename);
        b->setText(QStringLiteral("%1 (%2)").arg(name, QLocale().formattedDataSize(static_cast<qint64>(a.size))));
        b->setIcon(QIcon::fromTheme(QStringLiteral("mail-attachment")));
        b->setToolButtonStyle(Qt::ToolButtonTextBesideIcon);
        b->setToolTip(tr("Open %1 (%2)").arg(name, qs(a.content_type)));
        const quint32 index = a.index;
        // Click opens; the arrow offers Save As.
        auto *menu = new QMenu(b);
        menu->addAction(QIcon::fromTheme(QStringLiteral("document-open")), tr("Open"), this,
                        [this, index, name] { openAttachment(index, name); });
        menu->addAction(QIcon::fromTheme(QStringLiteral("document-save-as")), tr("Save As…"), this,
                        [this, index, name] { saveAttachment(index, name); });
        b->setMenu(menu);
        b->setPopupMode(QToolButton::MenuButtonPopup);
        connect(b, &QToolButton::clicked, this, [this, index, name] { openAttachment(index, name); });
        m_attachmentLayout->addWidget(b);
    }
    m_attachmentLayout->addStretch(1);
    m_attachmentBar->setVisible(!view->attachments.empty());

    if (!sameMessage)
        m_plain->setChecked(false);
    m_plain->setEnabled(view->has_html);
    load();
}

void MessageView::load()
{
    if (!m_view)
        return;
    if (m_view->body_missing) {
        m_interceptor->setAllowRemote(false);
        m_web->setHtml(QStringLiteral("<p style='font-family:sans-serif;color:gray'>%1</p>")
                           .arg(tr("Downloading message…").toHtmlEscaped()));
        return;
    }
    const bool plain = m_plain->isChecked();
    m_interceptor->setAllowRemote(m_view->remote_allowed && !plain);
    m_web->load(QUrl(qs(plain ? m_view->text_url : m_view->url)));
}

void MessageView::saveAttachment(quint32 index, const QString &filename)
{
    const auto res = core().attachment(m_key.toFfi(), index);
    if (!res.found) {
        QMessageBox::warning(this, tr("Save Attachment"), tr("The attachment is no longer available."));
        return;
    }
    // Only the file name part; never trust paths from mail.
    const QString safeName = QFileInfo(filename).fileName();
    const QString dir = QStandardPaths::writableLocation(QStandardPaths::DownloadLocation);
    const QString path = QFileDialog::getSaveFileName(this, tr("Save Attachment"), dir + u'/' + safeName);
    if (path.isEmpty())
        return;
    QFile file(path);
    if (!file.open(QIODevice::WriteOnly)
        || file.write(reinterpret_cast<const char *>(res.data.data()), static_cast<qint64>(res.data.size()))
               != static_cast<qint64>(res.data.size())) {
        QMessageBox::warning(this, tr("Save Attachment"), tr("Could not write %1: %2").arg(path, file.errorString()));
    }
}

void MessageView::openAttachment(quint32 index, const QString &filename)
{
    const auto res = core().attachment(m_key.toFfi(), index);
    if (!res.found) {
        QMessageBox::warning(this, tr("Open Attachment"), tr("The attachment is no longer available."));
        return;
    }
    // A fresh directory per open, so equal names never clash; only the file
    // name part of the attachment's name is used.
    QString safeName = QFileInfo(filename).fileName();
    if (safeName.isEmpty() || safeName.startsWith(u'.'))
        safeName.prepend(QStringLiteral("attachment"));
    const QString dir = openedAttachmentsDir() + u'/' + QString::number(QDateTime::currentMSecsSinceEpoch(), 36);
    if (!QDir().mkpath(dir)) {
        QMessageBox::warning(this, tr("Open Attachment"), tr("Could not create %1.").arg(dir));
        return;
    }
    QFile file(dir + u'/' + safeName);
    // Read-only for the user: it's a snapshot, edits wouldn't go anywhere.
    if (!file.open(QIODevice::WriteOnly | QIODevice::NewOnly)
        || file.write(reinterpret_cast<const char *>(res.data.data()), static_cast<qint64>(res.data.size()))
               != static_cast<qint64>(res.data.size())) {
        QMessageBox::warning(this, tr("Open Attachment"), tr("Could not write %1: %2").arg(file.fileName(), file.errorString()));
        return;
    }
    file.close();
    file.setPermissions(QFileDevice::ReadOwner);
    openFileExternally(file.fileName());
}

void MessageView::showContextMenu(const QPoint &pos)
{
    QMenu menu(this);
    const QWebEngineContextMenuRequest *req = m_web->lastContextMenuRequest();
    if (req && !req->selectedText().isEmpty())
        menu.addAction(QIcon::fromTheme(QStringLiteral("edit-copy")), tr("Copy"), this,
                       [this] { m_web->triggerPageAction(QWebEnginePage::Copy); });
    if (req && req->linkUrl().isValid()) {
        const QUrl link = req->linkUrl();
        menu.addAction(QIcon::fromTheme(QStringLiteral("edit-copy")), tr("Copy Link Address"), this,
                       [link] { QGuiApplication::clipboard()->setText(link.toString()); });
    }
    menu.addAction(QIcon::fromTheme(QStringLiteral("edit-select-all")), tr("Select All"), this,
                   [this] { m_web->triggerPageAction(QWebEnginePage::SelectAll); });
    menu.exec(m_web->mapToGlobal(pos));
}

} // namespace tern
