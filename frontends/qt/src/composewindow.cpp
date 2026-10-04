#include "composewindow.h"

#include <QAction>
#include <QCheckBox>
#include <QCloseEvent>
#include <QComboBox>
#include <QFileDialog>
#include <QFileInfo>
#include <QFontDatabase>
#include <QFormLayout>
#include <QInputDialog>
#include <QLabel>
#include <QLineEdit>
#include <QListWidget>
#include <QMessageBox>
#include <QPlainTextEdit>
#include <QShortcut>
#include <QStackedWidget>
#include <QStandardPaths>
#include <QTextBrowser>
#include <QTextCharFormat>
#include <QTextEdit>
#include <QTextList>
#include <QToolBar>
#include <QVBoxLayout>

namespace tern {

namespace {

int formatIndex(ffi::BodyFormat f)
{
    switch (f) {
    case ffi::BodyFormat::Markdown:
        return 1;
    case ffi::BodyFormat::Html:
        return 2;
    default:
        return 0;
    }
}

ffi::BodyFormat formatAt(int index)
{
    switch (index) {
    case 1:
        return ffi::BodyFormat::Markdown;
    case 2:
        return ffi::BodyFormat::Html;
    default:
        return ffi::BodyFormat::Plain;
    }
}

} // namespace

ComposeWindow::ComposeWindow(const ffi::Draft &draft, QWidget *parent) : QWidget(parent, Qt::Window), m_draft(draft)
{
    setAttribute(Qt::WA_DeleteOnClose);
    resize(820, 700);

    // Main toolbar: send, attach, editor mode, PGP.
    auto *toolbar = new QToolBar(this);
    toolbar->setToolButtonStyle(Qt::ToolButtonTextBesideIcon);
    QAction *send = toolbar->addAction(QIcon::fromTheme(QStringLiteral("mail-send")), tr("Send"), this,
                                       &ComposeWindow::send);
    send->setShortcut(QKeySequence(Qt::CTRL | Qt::Key_Return));
    send->setToolTip(tr("Send (Ctrl+Return)"));
    toolbar->addAction(QIcon::fromTheme(QStringLiteral("mail-attachment")), tr("Attach…"), this,
                       &ComposeWindow::addAttachments);
    toolbar->addSeparator();
    toolbar->addWidget(new QLabel(tr("Format: "), toolbar));
    m_formatBox = new QComboBox(toolbar);
    m_formatBox->addItems({tr("Plain text"), tr("Markdown"), tr("HTML")});
    m_formatBox->setToolTip(tr("Editor mode. Switching converts the text."));
    toolbar->addWidget(m_formatBox);
    toolbar->addSeparator();
    m_sign = new QCheckBox(tr("Sign"), toolbar);
    m_encrypt = new QCheckBox(tr("Encrypt"), toolbar);
    toolbar->addWidget(m_sign);
    toolbar->addWidget(m_encrypt);

    // Rich-text formatting (HTML mode only).
    m_richBar = new QToolBar(this);
    m_richBar->setIconSize(QSize(16, 16));
    auto fmtAction = [this](const QString &icon, const QString &text, const QKeySequence &key) {
        QAction *a = m_richBar->addAction(QIcon::fromTheme(icon), text);
        a->setCheckable(true);
        a->setShortcut(key);
        a->setToolTip(QStringLiteral("%1 (%2)").arg(text, key.toString(QKeySequence::NativeText)));
        return a;
    };
    m_boldAct = fmtAction(QStringLiteral("format-text-bold"), tr("Bold"), QKeySequence::Bold);
    m_italicAct = fmtAction(QStringLiteral("format-text-italic"), tr("Italic"), QKeySequence::Italic);
    m_underlineAct = fmtAction(QStringLiteral("format-text-underline"), tr("Underline"), QKeySequence::Underline);
    m_strikeAct = fmtAction(QStringLiteral("format-text-strikethrough"), tr("Strikethrough"), QKeySequence());
    connect(m_boldAct, &QAction::triggered, this, [this](bool on) {
        QTextCharFormat f;
        f.setFontWeight(on ? QFont::Bold : QFont::Normal);
        mergeFormat(f);
    });
    connect(m_italicAct, &QAction::triggered, this, [this](bool on) {
        QTextCharFormat f;
        f.setFontItalic(on);
        mergeFormat(f);
    });
    connect(m_underlineAct, &QAction::triggered, this, [this](bool on) {
        QTextCharFormat f;
        f.setFontUnderline(on);
        mergeFormat(f);
    });
    connect(m_strikeAct, &QAction::triggered, this, [this](bool on) {
        QTextCharFormat f;
        f.setFontStrikeOut(on);
        mergeFormat(f);
    });
    m_richBar->addSeparator();
    m_richBar->addAction(QIcon::fromTheme(QStringLiteral("format-list-unordered")), tr("Bulleted list"), this,
                         [this] { toggleList(false); });
    m_richBar->addAction(QIcon::fromTheme(QStringLiteral("format-list-ordered")), tr("Numbered list"), this,
                         [this] { toggleList(true); });
    m_richBar->addSeparator();
    QAction *link = m_richBar->addAction(QIcon::fromTheme(QStringLiteral("insert-link")), tr("Link…"), this,
                                         &ComposeWindow::insertLink);
    link->setShortcut(QKeySequence(Qt::CTRL | Qt::Key_K));
    m_richBar->addAction(QIcon::fromTheme(QStringLiteral("edit-clear-all")), tr("Clear formatting"), this,
                         &ComposeWindow::clearFormatting);
    // Icons when the theme has them all (Breeze does), text otherwise.
    bool allIcons = true;
    for (QAction *a : m_richBar->actions())
        allIcons &= a->isSeparator() || !a->icon().isNull();
    m_richBar->setToolButtonStyle(allIcons ? Qt::ToolButtonIconOnly : Qt::ToolButtonTextOnly);

    // Markdown: preview toggle.
    m_markdownBar = new QToolBar(this);
    m_previewAct = m_markdownBar->addAction(QIcon::fromTheme(QStringLiteral("document-preview")), tr("Preview"));
    m_previewAct->setCheckable(true);
    m_previewAct->setShortcut(QKeySequence(Qt::CTRL | Qt::SHIFT | Qt::Key_P));
    m_markdownBar->setToolButtonStyle(Qt::ToolButtonTextBesideIcon);
    connect(m_previewAct, &QAction::toggled, this, &ComposeWindow::togglePreview);
    m_markdownBar->addWidget(new QLabel(tr("  CommonMark: **bold**, *italic*, [link](url), - lists, > quotes"),
                                        m_markdownBar));

    // Header fields.
    m_from = new QComboBox(this);
    m_from->setSizePolicy(QSizePolicy::Expanding, QSizePolicy::Fixed);
    for (const auto &a : core().accounts())
        m_from->addItem(QStringLiteral("%1 <%2>").arg(qs(a.name), qs(a.email)), qs(a.id));
    const int current = m_from->findData(qs(draft.account));
    if (current >= 0)
        m_from->setCurrentIndex(current);
    // Replies and forwards keep the account of the original message.
    m_from->setEnabled(draft.reply_to_message.account.empty() && draft.forward_message.account.empty());

    m_to = new QLineEdit(qs(draft.to), this);
    m_cc = new QLineEdit(qs(draft.cc), this);
    m_bcc = new QLineEdit(qs(draft.bcc), this);
    m_subject = new QLineEdit(qs(draft.subject), this);
    for (QLineEdit *e : {m_to, m_cc, m_bcc})
        e->setPlaceholderText(tr("Name <address>, …"));

    auto *form = new QFormLayout;
    form->setFieldGrowthPolicy(QFormLayout::AllNonFixedFieldsGrow);
    form->addRow(tr("From:"), m_from);
    form->addRow(tr("To:"), m_to);
    form->addRow(tr("Cc:"), m_cc);
    form->addRow(tr("Bcc:"), m_bcc);
    form->addRow(tr("Subject:"), m_subject);

    // Editors.
    m_text = new QPlainTextEdit(this);
    m_text->setFont(QFontDatabase::systemFont(QFontDatabase::FixedFont));
    m_text->setLineWrapMode(QPlainTextEdit::WidgetWidth);
    m_rich = new QTextEdit(this);
    m_rich->setAcceptRichText(true);
    m_rich->setAutoFormatting(QTextEdit::AutoBulletList);
    connect(m_rich, &QTextEdit::currentCharFormatChanged, this, &ComposeWindow::syncFormatActions);
    m_preview = new QTextBrowser(this);
    m_preview->setOpenLinks(false);
    m_editors = new QStackedWidget(this);
    m_editors->addWidget(m_text);
    m_editors->addWidget(m_rich);
    m_editors->addWidget(m_preview);

    m_attachments = new QListWidget(this);
    m_attachments->setMaximumHeight(80);
    m_attachments->setFlow(QListView::LeftToRight);
    m_attachments->setToolTip(tr("Press Delete to remove an attachment"));
    for (const auto &path : draft.attachments)
        m_attachments->addItem(qs(path));
    m_attachments->setVisible(m_attachments->count() > 0);
    auto *removeAttachment = new QShortcut(QKeySequence::Delete, m_attachments, nullptr, nullptr, Qt::WidgetShortcut);
    connect(removeAttachment, &QShortcut::activated, this, [this] {
        delete m_attachments->currentItem();
        m_attachments->setVisible(m_attachments->count() > 0);
    });

    auto *layout = new QVBoxLayout(this);
    layout->setContentsMargins(0, 0, 0, 0);
    layout->setSpacing(0);
    layout->addWidget(toolbar);
    auto *inner = new QVBoxLayout;
    inner->setContentsMargins(8, 4, 8, 8);
    inner->addLayout(form);
    inner->addWidget(m_attachments);
    inner->addWidget(m_richBar);
    inner->addWidget(m_markdownBar);
    inner->addWidget(m_editors, 1);
    layout->addLayout(inner);

    setBody(qs(draft.body), draft.format);
    m_initialBody = body();
    m_sign->setChecked(draft.sign);
    m_encrypt->setChecked(draft.encrypt);
    for (const auto &a : core().accounts())
        if (qs(a.id) == qs(draft.account)) {
            m_sign->setEnabled(a.has_pgp_key);
            m_sign->setToolTip(a.has_pgp_key ? QString() : tr("No PGP key configured for this account"));
        }
    connect(m_formatBox, &QComboBox::currentIndexChanged, this, &ComposeWindow::formatSelected);
    connect(m_from, &QComboBox::currentIndexChanged, this, &ComposeWindow::accountChanged);

    const QString subject = m_subject->text();
    setWindowTitle(subject.isEmpty() ? tr("New Message") : subject);
    connect(m_subject, &QLineEdit::textChanged, this,
            [this](const QString &t) { setWindowTitle(t.isEmpty() ? tr("New Message") : t); });

    connect(bridge(), &EventBridge::draftQueued, this, [this](quint64 request, const QString &error) {
        if (request != m_pending)
            return;
        m_pending = 0;
        if (error.isEmpty()) {
            m_sent = true;
            close();
            return;
        }
        show();
        raise();
        activateWindow();
        QMessageBox::warning(this, tr("Message Not Sent"), error);
    });

    if (m_to->text().isEmpty()) {
        m_to->setFocus();
    } else if (m_format == ffi::BodyFormat::Html) {
        m_rich->setFocus();
        m_rich->moveCursor(QTextCursor::Start);
    } else {
        m_text->setFocus();
        m_text->moveCursor(QTextCursor::Start);
    }
}

// ---------------------------------------------------------- editor modes

QString ComposeWindow::body() const
{
    return m_format == ffi::BodyFormat::Html ? m_rich->toHtml() : m_text->toPlainText();
}

void ComposeWindow::setBody(const QString &body, ffi::BodyFormat format)
{
    m_format = format;
    if (format == ffi::BodyFormat::Html)
        m_rich->setHtml(body);
    else
        m_text->setPlainText(body);
    {
        QSignalBlocker block(m_formatBox);
        m_formatBox->setCurrentIndex(formatIndex(format));
    }
    m_previewAct->setChecked(false);
    updateModeUi();
}

void ComposeWindow::formatSelected(int index)
{
    const ffi::BodyFormat to = formatAt(index);
    if (to == m_format)
        return;
    if (m_format == ffi::BodyFormat::Html && to == ffi::BodyFormat::Plain
        && QMessageBox::question(this, tr("Switch to Plain Text"),
                                 tr("Switching to plain text removes all formatting. Continue?"))
               != QMessageBox::Yes) {
        QSignalBlocker block(m_formatBox);
        m_formatBox->setCurrentIndex(formatIndex(m_format));
        return;
    }
    const bool wasModified = modified();
    const QString converted = qs(core().convert_body(rs(body()), m_format, to));
    setBody(converted, to);
    // A pure mode switch isn't an edit.
    if (!wasModified)
        m_initialBody = body();
    (to == ffi::BodyFormat::Html ? static_cast<QWidget *>(m_rich) : m_text)->setFocus();
}

void ComposeWindow::updateModeUi()
{
    const bool html = m_format == ffi::BodyFormat::Html;
    m_richBar->setVisible(html);
    m_markdownBar->setVisible(m_format == ffi::BodyFormat::Markdown);
    m_editors->setCurrentWidget(html ? static_cast<QWidget *>(m_rich) : m_text);
}

void ComposeWindow::togglePreview(bool on)
{
    if (on && m_format == ffi::BodyFormat::Markdown) {
        m_preview->setHtml(qs(core().markdown_preview(rs(m_text->toPlainText()))));
        m_editors->setCurrentWidget(m_preview);
    } else {
        updateModeUi();
        m_text->setFocus();
    }
}

// ------------------------------------------------------- rich formatting

void ComposeWindow::mergeFormat(const QTextCharFormat &fmt)
{
    QTextCursor cursor = m_rich->textCursor();
    if (!cursor.hasSelection())
        cursor.select(QTextCursor::WordUnderCursor);
    cursor.mergeCharFormat(fmt);
    m_rich->mergeCurrentCharFormat(fmt);
    m_rich->setFocus();
}

void ComposeWindow::toggleList(bool numbered)
{
    QTextCursor cursor = m_rich->textCursor();
    const auto style = numbered ? QTextListFormat::ListDecimal : QTextListFormat::ListDisc;
    if (QTextList *list = cursor.currentList(); list && list->format().style() == style) {
        // Already this kind of list: turn the block back into a paragraph.
        list->remove(cursor.block());
        QTextBlockFormat bf = cursor.blockFormat();
        bf.setIndent(0);
        cursor.setBlockFormat(bf);
    } else {
        cursor.createList(style);
    }
    m_rich->setFocus();
}

void ComposeWindow::insertLink()
{
    QTextCursor cursor = m_rich->textCursor();
    bool ok = false;
    const QString url = QInputDialog::getText(this, tr("Insert Link"), tr("Address:"), QLineEdit::Normal,
                                              QStringLiteral("https://"), &ok)
                            .trimmed();
    if (!ok || url.isEmpty() || url == u"https://")
        return;
    QTextCharFormat f;
    f.setAnchor(true);
    f.setAnchorHref(url);
    f.setFontUnderline(true);
    f.setForeground(palette().link());
    if (cursor.hasSelection())
        cursor.mergeCharFormat(f);
    else
        cursor.insertText(url, f);
    m_rich->setFocus();
}

void ComposeWindow::clearFormatting()
{
    QTextCursor cursor = m_rich->textCursor();
    if (cursor.hasSelection())
        cursor.setCharFormat(QTextCharFormat());
    m_rich->setCurrentCharFormat(QTextCharFormat());
    syncFormatActions();
}

void ComposeWindow::syncFormatActions()
{
    const QTextCharFormat f = m_rich->currentCharFormat();
    m_boldAct->setChecked(f.fontWeight() >= QFont::Bold);
    m_italicAct->setChecked(f.fontItalic());
    m_underlineAct->setChecked(f.fontUnderline());
    m_strikeAct->setChecked(f.fontStrikeOut());
}

// ---------------------------------------------------------------- the rest

void ComposeWindow::accountChanged()
{
    const QString id = m_from->currentData().toString();
    for (const auto &a : core().accounts()) {
        if (qs(a.id) != id)
            continue;
        m_sign->setEnabled(a.has_pgp_key);
        m_sign->setToolTip(a.has_pgp_key ? QString() : tr("No PGP key configured for this account"));
    }
    // An untouched new message follows the account: its signature, editor
    // mode and PGP defaults. Edited text is never replaced.
    if (!modified()) {
        const ffi::Draft fresh = core().new_draft(rs(id));
        setBody(qs(fresh.body), fresh.format);
        m_initialBody = body();
        m_sign->setChecked(fresh.sign && m_sign->isEnabled());
        m_encrypt->setChecked(fresh.encrypt);
    } else if (!m_sign->isEnabled()) {
        m_sign->setChecked(false);
    }
}

void ComposeWindow::addAttachments()
{
    const QStringList files = QFileDialog::getOpenFileNames(
        this, tr("Attach Files"), QStandardPaths::writableLocation(QStandardPaths::HomeLocation));
    for (const QString &f : files)
        m_attachments->addItem(f);
    m_attachments->setVisible(m_attachments->count() > 0);
}

bool ComposeWindow::modified() const
{
    return !m_sent && body() != m_initialBody;
}

void ComposeWindow::send()
{
    if (m_to->text().trimmed().isEmpty() && m_cc->text().trimmed().isEmpty() && m_bcc->text().trimmed().isEmpty()) {
        QMessageBox::warning(this, tr("Send"), tr("Please enter at least one recipient."));
        m_to->setFocus();
        return;
    }
    if (m_subject->text().trimmed().isEmpty()
        && QMessageBox::question(this, tr("Send"), tr("Send this message without a subject?")) != QMessageBox::Yes)
        return;

    ffi::Draft d = m_draft;
    d.account = rs(m_from->currentData().toString());
    d.to = rs(m_to->text());
    d.cc = rs(m_cc->text());
    d.bcc = rs(m_bcc->text());
    d.subject = rs(m_subject->text());
    d.body = rs(body());
    d.format = m_format;
    d.sign = m_sign->isChecked();
    d.encrypt = m_encrypt->isChecked();
    d.attachments.clear();
    for (int i = 0; i < m_attachments->count(); ++i)
        d.attachments.push_back(rs(m_attachments->item(i)->text()));
    // Building (and gpg, which may ask for a passphrase) runs in the core.
    // Hide meanwhile; come back if the message can't be built.
    m_pending = core().send(std::move(d));
    hide();
}

void ComposeWindow::closeEvent(QCloseEvent *event)
{
    if (m_pending == 0 && modified()
        && QMessageBox::question(this, tr("Discard Message"), tr("Discard this message?"),
                                 QMessageBox::Discard | QMessageBox::Cancel)
               != QMessageBox::Discard) {
        event->ignore();
        return;
    }
    event->accept();
}

} // namespace tern
