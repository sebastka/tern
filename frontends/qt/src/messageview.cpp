#include "messageview.h"

#include "messagebar.h"
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
#include <QSignalBlocker>
#include <QPainter>
#include <QPainterPath>
#include <QImageReader>
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

static QPixmap photo(const QByteArray &data, int size, qreal dpr);

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
    // A photo looked up in the background for the sender on screen.
    connect(bridge(), &EventBridge::avatarReady, this, [this](const QString &email, const QByteArray &image) {
        if (!m_view || email.compare(qs(m_view->sender.email), Qt::CaseInsensitive) != 0)
            return;
        if (const QPixmap pic = photo(image, 40, devicePixelRatioF()); !pic.isNull())
            m_avatar->setPixmap(pic);
    });
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
    // Same kind of control as Headers and Source: a toggle button.
    m_plain = new QToolButton(m_header);
    m_plain->setText(tr("Plain text"));
    m_plain->setIcon(QIcon::fromTheme(QStringLiteral("text-plain"), QIcon::fromTheme(QStringLiteral("text-x-generic"))));
    m_plain->setToolButtonStyle(Qt::ToolButtonTextBesideIcon);
    m_plain->setAutoRaise(true);
    m_plain->setCheckable(true);
    m_plain->setToolTip(tr("Show the plain-text version"));
    connect(m_plain, &QToolButton::toggled, this, &MessageView::load);
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

    // Sender block: [avatar] Name ............ date
    //                        address
    //                        to Ann, Bob, +3
    m_senderBlock = new QWidget(m_header);
    auto *sl = new QHBoxLayout(m_senderBlock);
    sl->setContentsMargins(0, 2, 0, 2);
    sl->setSpacing(10);
    m_avatar = new QLabel(m_senderBlock);
    m_avatar->setFixedSize(40, 40);
    sl->addWidget(m_avatar, 0, Qt::AlignTop);
    auto *lines = new QVBoxLayout;
    lines->setSpacing(1);
    auto *nameRow = new QHBoxLayout;
    m_senderName = new QLabel(m_senderBlock);
    QFont nf = m_senderName->font();
    nf.setBold(true);
    nf.setPointSizeF(nf.pointSizeF() * 1.05);
    m_senderName->setFont(nf);
    m_senderName->setTextInteractionFlags(Qt::TextSelectableByMouse);
    m_date = new QLabel(m_senderBlock);
    m_date->setTextInteractionFlags(Qt::TextSelectableByMouse);
    nameRow->addWidget(m_senderName, 1);
    nameRow->addWidget(m_date, 0, Qt::AlignRight | Qt::AlignTop);
    lines->addLayout(nameRow);
    m_senderEmail = new QLabel(m_senderBlock);
    m_senderEmail->setTextInteractionFlags(Qt::TextSelectableByMouse);
    lines->addWidget(m_senderEmail);
    m_recipients = new QLabel(m_senderBlock);
    m_recipients->setWordWrap(true);
    m_recipients->setTextFormat(Qt::RichText);
    m_recipients->setTextInteractionFlags(Qt::TextBrowserInteraction);
    m_recipients->setOpenExternalLinks(false);
    connect(m_recipients, &QLabel::linkActivated, this, [this](const QString &link) {
        if (link == u"toggle") {
            m_recipientsExpanded = !m_recipientsExpanded;
            updateRecipients();
        }
    });
    lines->addWidget(m_recipients);
    sl->addLayout(lines, 1);
    // Secondary lines in the palette's placeholder color, a bit smaller.
    for (QLabel *l : {m_senderEmail, m_date, m_recipients}) {
        QPalette p = l->palette();
        p.setColor(QPalette::WindowText, p.color(QPalette::PlaceholderText));
        l->setPalette(p);
        QFont f = l->font();
        f.setPointSizeF(f.pointSizeF() * 0.92);
        l->setFont(f);
    }
    hl->addWidget(m_senderBlock);
    // Plain rich text only: no links are followed, no resources loaded.
    m_allHeaders = new QTextBrowser(m_header);
    m_allHeaders->setOpenLinks(false);
    m_allHeaders->setOpenExternalLinks(false);
    m_allHeaders->setWordWrapMode(QTextOption::WrapAtWordBoundaryOrAnywhere);
    m_allHeaders->setMaximumHeight(240);
    m_allHeaders->hide();
    hl->addWidget(m_allHeaders);
    m_securityBar = new MessageBar(m_header);
    hl->addWidget(m_securityBar);

    m_remoteBar = new MessageBar(m_header);
    m_remoteBar->setMessage(MessageBar::Information, tr("Remote content was blocked to protect your privacy."));
    auto *load = new QPushButton(tr("Load remote content"), m_remoteBar);
    connect(load, &QPushButton::clicked, this, [this] {
        if (m_key.valid())
            core().open_message(m_key.toFfi(), true);
    });
    m_remoteBar->addButton(load);
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
    m_senderBlock->setVisible(!all);
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

// Initials on a circle whose color is derived from the address, so a
// sender keeps the same color everywhere.
static QPixmap avatar(const QString &name, const QString &email, int size, qreal dpr)
{
    QString initials;
    for (const QString &word : name.split(u' ', Qt::SkipEmptyParts)) {
        const QChar c = word.front();
        if (c.isLetterOrNumber())
            initials += c.toUpper();
        if (initials.size() == 2)
            break;
    }
    if (initials.isEmpty() && !email.isEmpty())
        initials = email.front().toUpper();
    const int hue = static_cast<int>(qHash(email.toLower()) % 360);
    QPixmap pm(QSize(size, size) * dpr);
    pm.setDevicePixelRatio(dpr);
    pm.fill(Qt::transparent);
    QPainter p(&pm);
    p.setRenderHint(QPainter::Antialiasing);
    p.setPen(Qt::NoPen);
    p.setBrush(QColor::fromHsv(hue, 110, 175));
    p.drawEllipse(QRectF(0, 0, size, size));
    QFont f = p.font();
    f.setBold(true);
    f.setPixelSize(size * 2 / 5);
    p.setFont(f);
    p.setPen(Qt::white);
    p.drawText(QRectF(0, 0, size, size), Qt::AlignCenter, initials);
    return pm;
}

// A sender photo (PNG/JPEG/GIF, checked by the core) cropped to a circle.
// Null if it can't be decoded or claims absurd dimensions (a small file
// can still decode to gigabytes).
static QPixmap photo(const QByteArray &data, int size, qreal dpr)
{
    QBuffer buffer;
    buffer.setData(data);
    buffer.open(QIODevice::ReadOnly);
    QImageReader reader(&buffer);
    const QSize natural = reader.size();
    if (!natural.isValid() || natural.width() > 4096 || natural.height() > 4096)
        return {};
    const int px = qRound(size * dpr);
    // Cover the circle: scale the short side to it while decoding.
    reader.setScaledSize(natural.scaled(px, px, Qt::KeepAspectRatioByExpanding));
    const QImage img = reader.read();
    if (img.isNull())
        return {};
    QPixmap pm(px, px);
    pm.fill(Qt::transparent);
    QPainter p(&pm);
    p.setRenderHint(QPainter::Antialiasing);
    p.setRenderHint(QPainter::SmoothPixmapTransform);
    QPainterPath circle;
    circle.addEllipse(QRectF(0, 0, px, px));
    p.setClipPath(circle);
    p.drawImage(QPointF((px - img.width()) / 2.0, (px - img.height()) / 2.0), img);
    p.end();
    pm.setDevicePixelRatio(dpr);
    return pm;
}

static QString personText(const ffi::Person &p, bool withAddress)
{
    const QString name = qs(p.name), email = qs(p.email);
    if (name.isEmpty())
        return escaped(email);
    return withAddress ? QStringLiteral("%1 &lt;%2&gt;").arg(escaped(name), escaped(email)) : escaped(name);
}

void MessageView::updateRecipients()
{
    if (!m_view)
        return;
    const auto &to = m_view->to_people;
    const auto &cc = m_view->cc_people;
    const qsizetype total = static_cast<qsizetype>(to.size() + cc.size());
    if (total == 0) {
        m_recipients->setText(QString());
        return;
    }
    QString html;
    if (m_recipientsExpanded) {
        QStringList t, c;
        for (const auto &p : to)
            t << personText(p, true);
        for (const auto &p : cc)
            c << personText(p, true);
        if (!t.isEmpty())
            html += tr("To: %1").arg(t.join(QStringLiteral(", ")));
        if (!c.isEmpty())
            html += (html.isEmpty() ? QString() : QStringLiteral("<br>")) + tr("Cc: %1").arg(c.join(QStringLiteral(", ")));
        html += QStringLiteral(" &nbsp;<a href=\"toggle\">%1</a>").arg(tr("less"));
    } else {
        // Up to two names, then a count; the link shows everything.
        QStringList names;
        for (const auto &p : to)
            if (names.size() < 2)
                names << personText(p, false);
        for (const auto &p : cc)
            if (names.size() < 2)
                names << personText(p, false);
        html = tr("to %1").arg(names.join(QStringLiteral(", ")));
        const qsizetype more = total - names.size();
        if (more > 0)
            html += QStringLiteral(", <a href=\"toggle\">%1</a>").arg(tr("+%1 more").arg(more));
        else
            html += QStringLiteral(" &nbsp;<a href=\"toggle\">%1</a>").arg(tr("details"));
    }
    m_recipients->setText(html);
}

void MessageView::showMessage(const std::shared_ptr<ffi::MessageView> &view)
{
    const Key key = Key::from(view->key);
    const bool sameMessage = key == m_key;
    m_key = key;
    m_view = view;
    m_header->show();

    m_subject->setText(qs(view->subject).isEmpty() ? tr("(no subject)") : qs(view->subject));
    const QString senderName = qs(view->sender.name), senderEmail = qs(view->sender.email);
    // Fall back to the raw From text if the sender couldn't be parsed.
    m_senderName->setText(!senderName.isEmpty() ? senderName
                          : !senderEmail.isEmpty() ? senderEmail
                                                   : qs(view->from));
    m_senderEmail->setText(senderName.isEmpty() ? QString() : senderEmail);
    m_senderEmail->setVisible(!senderName.isEmpty() && !senderEmail.isEmpty());
    // The sender's photo if the core has one (`[avatars]`), else initials.
    const QPixmap pic = view->avatar.empty()
                            ? QPixmap()
                            : photo(QByteArray(reinterpret_cast<const char *>(view->avatar.data()),
                                               static_cast<qsizetype>(view->avatar.size())),
                                    40, devicePixelRatioF());
    m_avatar->setPixmap(!pic.isNull() ? pic
                                      : avatar(senderName, senderEmail.isEmpty() ? qs(view->from) : senderEmail, 40,
                                               devicePixelRatioF()));
    const QDateTime when = QDateTime::fromSecsSinceEpoch(view->date).toLocalTime();
    m_date->setText(QLocale().toString(when, QLocale::ShortFormat));
    m_date->setToolTip(QLocale().toString(when, QLocale::LongFormat));
    if (!sameMessage)
        m_recipientsExpanded = false;
    updateRecipients();

    QString all = QStringLiteral("<table cellspacing='0' cellpadding='1'>");
    for (const auto &h : view->headers)
        all += QStringLiteral("<tr><td valign='top' style='white-space:pre'><b>%1:</b> </td><td>%2</td></tr>")
                   .arg(escaped(qs(h.name)), escaped(qs(h.value)));
    all += QStringLiteral("</table>");
    m_allHeaders->setHtml(all);
    updateHeaderView();

    // Security bar: encryption and signature state, as decided in Rust.
    QString sec;
    MessageBar::Kind kind = MessageBar::Information;
    if (view->decryption_failed) {
        sec = tr("This message is encrypted and could not be decrypted.");
        kind = MessageBar::Error;
    } else {
        QStringList parts;
        if (view->encrypted)
            parts << tr("Encrypted");
        switch (view->signature) {
        case ffi::SignatureState::Good:
            kind = MessageBar::Positive;
            break;
        case ffi::SignatureState::Warning:
        case ffi::SignatureState::Unknown:
            kind = MessageBar::Warning;
            break;
        case ffi::SignatureState::Bad:
            kind = MessageBar::Error;
            break;
        default:
            break;
        }
        if (!view->signature_text.empty())
            parts << qs(view->signature_text);
        sec = parts.join(QStringLiteral(" — "));
    }
    m_securityBar->setVisible(!sec.isEmpty());
    if (!sec.isEmpty())
        m_securityBar->setMessage(kind, sec);

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

    {
        // A new message starts from `prefer_plain_text`; the button switches
        // between the two versions (HTML stays reachable). No load() from
        // the toggled signal here: it's called once below.
        const QSignalBlocker block(m_plain);
        if (!sameMessage)
            m_plain->setChecked(view->has_html && core().prefer_plain_text());
        if (!view->has_html)
            m_plain->setChecked(false);
        m_plain->setEnabled(view->has_html);
        m_plain->setToolTip(view->has_html ? tr("Show the plain-text version")
                                           : tr("This message has no HTML version"));
    }
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
