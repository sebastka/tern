// "View Source": the message exactly as received, colored by line class.
// The classes come from the core (tern-app); nothing is parsed here (§3).
#pragma once

#include "core.h"

#include <QList>
#include <QSyntaxHighlighter>
#include <QWidget>

class QPlainTextEdit;

namespace tern {

// Open a source window for `key`, or explain why it can't be shown.
// `subject` is only used for the window title.
void showMessageSource(QWidget *parent, const Key &key, const QString &subject);

class SourceHighlighter : public QSyntaxHighlighter {
    Q_OBJECT
public:
    SourceHighlighter(QTextDocument *document, QList<ffi::SourceLine> lines, const QPalette &palette);

protected:
    void highlightBlock(const QString &text) override;

private:
    QList<ffi::SourceLine> m_lines;
    QTextCharFormat m_name;
    QTextCharFormat m_boundary;
    QTextCharFormat m_encoded;
    QTextCharFormat m_quote;
    QTextCharFormat m_armor;
};

class SourceWindow : public QWidget {
    Q_OBJECT
public:
    SourceWindow(ffi::MessageSource source, const QString &subject);

private:
    void saveAs();

    QByteArray m_raw;
    QString m_fileName;
    QPlainTextEdit *m_text = nullptr;
};

} // namespace tern
