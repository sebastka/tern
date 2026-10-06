// Inline message bar (icon, text, optional buttons) on a tinted, rounded
// background, in the spirit of KDE's message widgets without depending on
// KDE Frameworks. Colors are mixed from the current palette, so it works in
// light and dark themes.
#pragma once

#include <QFrame>

class QAbstractButton;
class QHBoxLayout;
class QLabel;

namespace tern {

class MessageBar : public QFrame {
    Q_OBJECT
public:
    enum Kind { Information, Positive, Warning, Error };

    explicit MessageBar(QWidget *parent = nullptr);

    void setMessage(Kind kind, const QString &text);
    // Buttons go to the right of the text.
    void addButton(QAbstractButton *button);

protected:
    void changeEvent(QEvent *event) override;
    void paintEvent(QPaintEvent *event) override;

private:
    QColor accent() const;
    void updateIcon();

    Kind m_kind = Information;
    QLabel *m_icon = nullptr;
    QLabel *m_text = nullptr;
    QHBoxLayout *m_layout = nullptr;
};

} // namespace tern
