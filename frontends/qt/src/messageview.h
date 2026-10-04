// Message display. The rendering policy is decided in Rust (tern-app); this
// widget only enforces it with QtWebEngine (§10).
#pragma once

#include "core.h"

#include <QWebEnginePage>
#include <QWebEngineUrlRequestInterceptor>
#include <QWebEngineUrlSchemeHandler>
#include <QWidget>

#include <atomic>
#include <memory>

class QLabel;
class QHBoxLayout;
class QPushButton;
class QCheckBox;
class QWebEngineView;
class QWebEngineProfile;

namespace tern {

inline constexpr auto Scheme = "tern-msg";

// Register the scheme; must run before QApplication is created.
void registerUrlScheme();

// Open a URL outside the app: OpenURI portal, falling back to QDesktopServices.
void openExternally(const QUrl &url);
// Open a local file with its default application (OpenURI portal OpenFile,
// falling back to QDesktopServices).
void openFileExternally(const QString &path);
// Where opened attachments are written; removed when Tern exits.
QString openedAttachmentsDir();

class SchemeHandler : public QWebEngineUrlSchemeHandler {
    Q_OBJECT
public:
    using QWebEngineUrlSchemeHandler::QWebEngineUrlSchemeHandler;
    void requestStarted(QWebEngineUrlRequestJob *job) override;
};

class RequestInterceptor : public QWebEngineUrlRequestInterceptor {
    Q_OBJECT
public:
    using QWebEngineUrlRequestInterceptor::QWebEngineUrlRequestInterceptor;
    void interceptRequest(QWebEngineUrlRequestInfo &info) override;
    void setAllowRemote(bool allow) { m_allowRemote = allow; }

private:
    std::atomic<bool> m_allowRemote{false};
};

class MessagePage : public QWebEnginePage {
    Q_OBJECT
public:
    using QWebEnginePage::QWebEnginePage;

Q_SIGNALS:
    void mailtoClicked(const QUrl &url);

protected:
    bool acceptNavigationRequest(const QUrl &url, NavigationType type, bool isMainFrame) override;
    QWebEnginePage *createWindow(WebWindowType type) override;
    void javaScriptConsoleMessage(JavaScriptConsoleMessageLevel, const QString &, int, const QString &) override {}
};

class MessageView : public QWidget {
    Q_OBJECT
public:
    explicit MessageView(QWidget *parent = nullptr);
    ~MessageView() override;

    void showMessage(const std::shared_ptr<ffi::MessageView> &view);
    void clear();
    Key currentKey() const { return m_key; }
    // Plain text of the displayed message (for quoting fallbacks).
    QString currentText() const;

Q_SIGNALS:
    void mailtoClicked(const QUrl &url);

private:
    void load();
    void saveAttachment(quint32 index, const QString &filename);
    void openAttachment(quint32 index, const QString &filename);
    void showContextMenu(const QPoint &pos);

    Key m_key;
    std::shared_ptr<ffi::MessageView> m_view;

    QWebEngineProfile *m_profile = nullptr;
    RequestInterceptor *m_interceptor = nullptr;
    MessagePage *m_page = nullptr;
    QWebEngineView *m_web = nullptr;

    QWidget *m_header = nullptr;
    QLabel *m_subject = nullptr;
    QLabel *m_meta = nullptr;
    QLabel *m_security = nullptr;
    QWidget *m_remoteBar = nullptr;
    QWidget *m_attachmentBar = nullptr;
    QHBoxLayout *m_attachmentLayout = nullptr;
    QCheckBox *m_plain = nullptr;
};

} // namespace tern
