// Composer with three editor modes: plain text, Markdown (with preview) and
// HTML (rich text). Building the message, format conversions, signing and
// encryption all happen in Rust.
#pragma once

#include "core.h"

#include <QWidget>

class QAction;
class QCheckBox;
class QComboBox;
class QLineEdit;
class QListWidget;
class QPlainTextEdit;
class QStackedWidget;
class QTextBrowser;
class QTextCharFormat;
class QTextEdit;
class QToolBar;

namespace tern {

class ComposeWindow : public QWidget {
    Q_OBJECT
public:
    explicit ComposeWindow(const ffi::Draft &draft, QWidget *parent = nullptr);

protected:
    void closeEvent(QCloseEvent *event) override;

private:
    void send();
    void addAttachments();
    void accountChanged();
    bool modified() const;

    // Editor modes.
    ffi::BodyFormat format() const { return m_format; }
    QString body() const;
    void setBody(const QString &body, ffi::BodyFormat format);
    void formatSelected(int index);
    void updateModeUi();
    void togglePreview(bool on);

    // Rich-text formatting (HTML mode).
    void mergeFormat(const QTextCharFormat &fmt);
    void toggleList(bool numbered);
    void insertLink();
    void clearFormatting();
    void syncFormatActions();

    ffi::Draft m_draft;
    ffi::BodyFormat m_format = ffi::BodyFormat::Plain;
    QComboBox *m_from = nullptr;
    QLineEdit *m_to = nullptr;
    QLineEdit *m_cc = nullptr;
    QLineEdit *m_bcc = nullptr;
    QLineEdit *m_subject = nullptr;
    QComboBox *m_formatBox = nullptr;
    QStackedWidget *m_editors = nullptr;
    QPlainTextEdit *m_text = nullptr; // plain text and Markdown
    QTextEdit *m_rich = nullptr;      // HTML
    QTextBrowser *m_preview = nullptr;
    QToolBar *m_richBar = nullptr;
    QToolBar *m_markdownBar = nullptr;
    QAction *m_previewAct = nullptr;
    QAction *m_boldAct = nullptr;
    QAction *m_italicAct = nullptr;
    QAction *m_underlineAct = nullptr;
    QAction *m_strikeAct = nullptr;
    QListWidget *m_attachments = nullptr;
    QCheckBox *m_sign = nullptr;
    QCheckBox *m_encrypt = nullptr;
    QString m_initialBody;
    bool m_sent = false;
    quint64 m_pending = 0; // request id while the core builds the message
};

} // namespace tern
