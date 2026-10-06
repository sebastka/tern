#include "sourcewindow.h"

#include <QAction>
#include <QCheckBox>
#include <QClipboard>
#include <QFile>
#include <QFileDialog>
#include <QFontDatabase>
#include <QGuiApplication>
#include <QHBoxLayout>
#include <QLabel>
#include <QLocale>
#include <QMessageBox>
#include <QPlainTextEdit>
#include <QPushButton>
#include <QStandardPaths>
#include <QVBoxLayout>

namespace tern {

// Above this, coloring would make opening noticeably slow.
static constexpr qsizetype MaxColoredLines = 200'000;

void showMessageSource(QWidget *parent, const Key &key, const QString &subject)
{
    if (!key.valid())
        return;
    ffi::MessageSource source = core().message_source(key.toFfi());
    if (!source.found) {
        QMessageBox::information(parent, QObject::tr("View Source"),
                                 QObject::tr("This message is not downloaded yet. The download has started; "
                                             "try again in a moment."));
        return;
    }
    auto *w = new SourceWindow(std::move(source), subject);
    w->show();
}

SourceHighlighter::SourceHighlighter(QTextDocument *document, QList<ffi::SourceLine> lines, const QPalette &palette)
    : QSyntaxHighlighter(document), m_lines(std::move(lines))
{
    // Readable on light and dark color schemes.
    const bool dark = palette.color(QPalette::Base).lightness() < 128;
    m_name.setFontWeight(QFont::Bold);
    m_name.setForeground(dark ? QColor(0x7c, 0xb7, 0xff) : QColor(0x1d, 0x5f, 0xa8));
    m_boundary.setFontWeight(QFont::Bold);
    m_boundary.setForeground(dark ? QColor(0xff, 0xb8, 0x6c) : QColor(0xb3, 0x59, 0x00));
    m_encoded.setForeground(palette.color(QPalette::PlaceholderText));
    m_quote.setForeground(dark ? QColor(0x8f, 0xd1, 0x8f) : QColor(0x2e, 0x7d, 0x32));
    m_armor.setFontWeight(QFont::Bold);
    m_armor.setForeground(dark ? QColor(0xd7, 0xa6, 0xff) : QColor(0x7b, 0x3f, 0xa0));
}

void SourceHighlighter::highlightBlock(const QString &text)
{
    const int line = currentBlock().blockNumber();
    if (line < 0 || line >= m_lines.size())
        return;
    switch (m_lines[line]) {
    case ffi::SourceLine::HeaderField: {
        // The name, colon included; the value keeps the normal color.
        const qsizetype colon = text.indexOf(u':');
        if (colon >= 0)
            setFormat(0, static_cast<int>(colon + 1), m_name);
        break;
    }
    case ffi::SourceLine::Boundary:
        setFormat(0, static_cast<int>(text.size()), m_boundary);
        break;
    case ffi::SourceLine::Encoded:
        setFormat(0, static_cast<int>(text.size()), m_encoded);
        break;
    case ffi::SourceLine::Quote:
        setFormat(0, static_cast<int>(text.size()), m_quote);
        break;
    case ffi::SourceLine::Armor:
        setFormat(0, static_cast<int>(text.size()), m_armor);
        break;
    default:
        break;
    }
}

SourceWindow::SourceWindow(ffi::MessageSource source, const QString &subject)
    : m_raw(reinterpret_cast<const char *>(source.raw.data()), static_cast<qsizetype>(source.raw.size())),
      m_fileName(qs(source.file_name))
{
    setAttribute(Qt::WA_DeleteOnClose);
    setWindowTitle(tr("Source: %1").arg(subject.isEmpty() ? tr("(no subject)") : subject));
    resize(900, 700);

    m_text = new QPlainTextEdit(this);
    m_text->setReadOnly(true);
    m_text->setFont(QFontDatabase::systemFont(QFontDatabase::FixedFont));
    m_text->setLineWrapMode(QPlainTextEdit::NoWrap);
    m_text->setUndoRedoEnabled(false);
    m_text->setPlainText(qs(source.text));
    if (static_cast<qsizetype>(source.lines.size()) <= MaxColoredLines) {
        QList<ffi::SourceLine> lines;
        lines.reserve(static_cast<qsizetype>(source.lines.size()));
        for (const auto l : source.lines)
            lines << l;
        new SourceHighlighter(m_text->document(), std::move(lines), m_text->palette());
    }

    auto *save = new QPushButton(QIcon::fromTheme(QStringLiteral("document-save-as")), tr("Save As…"), this);
    connect(save, &QPushButton::clicked, this, &SourceWindow::saveAs);
    auto *copy = new QPushButton(QIcon::fromTheme(QStringLiteral("edit-copy")), tr("Copy All"), this);
    connect(copy, &QPushButton::clicked, this,
            [this] { QGuiApplication::clipboard()->setText(m_text->toPlainText()); });
    auto *wrap = new QCheckBox(tr("Wrap lines"), this);
    connect(wrap, &QCheckBox::toggled, this, [this](bool on) {
        m_text->setLineWrapMode(on ? QPlainTextEdit::WidgetWidth : QPlainTextEdit::NoWrap);
    });
    auto *size = new QLabel(QLocale().formattedDataSize(m_raw.size()), this);
    size->setToolTip(tr("Saved exactly as received. Bytes that are not valid UTF-8 are shown as �."));

    auto *bar = new QHBoxLayout;
    bar->addWidget(save);
    bar->addWidget(copy);
    bar->addWidget(wrap);
    bar->addStretch(1);
    bar->addWidget(size);
    auto *layout = new QVBoxLayout(this);
    layout->addLayout(bar);
    layout->addWidget(m_text, 1);

    auto *saveKey = new QAction(this);
    saveKey->setShortcut(QKeySequence::Save);
    connect(saveKey, &QAction::triggered, this, &SourceWindow::saveAs);
    addAction(saveKey);
    auto *closeKey = new QAction(this);
    closeKey->setShortcuts(QList<QKeySequence>{QKeySequence(QKeySequence::Close), QKeySequence(Qt::Key_Escape)});
    connect(closeKey, &QAction::triggered, this, &QWidget::close);
    addAction(closeKey);
}

void SourceWindow::saveAs()
{
    const QString dir = QStandardPaths::writableLocation(QStandardPaths::DownloadLocation);
    const QString path = QFileDialog::getSaveFileName(this, tr("Save Message"), dir + u'/' + m_fileName,
                                                      tr("E-mail messages (*.eml);;All files (*)"));
    if (path.isEmpty())
        return;
    QFile file(path);
    if (!file.open(QIODevice::WriteOnly) || file.write(m_raw) != m_raw.size())
        QMessageBox::warning(this, tr("Save Message"), tr("Could not write %1: %2").arg(path, file.errorString()));
}

} // namespace tern
