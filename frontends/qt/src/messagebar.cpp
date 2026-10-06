#include "messagebar.h"

#include <QAbstractButton>
#include <QEvent>
#include <QHBoxLayout>
#include <QIcon>
#include <QLabel>
#include <QPainter>
#include <QStyle>

namespace tern {

MessageBar::MessageBar(QWidget *parent) : QFrame(parent)
{
    m_layout = new QHBoxLayout(this);
    m_layout->setContentsMargins(8, 4, 6, 4);
    m_layout->setSpacing(8);
    m_icon = new QLabel(this);
    m_text = new QLabel(this);
    m_text->setWordWrap(true);
    m_text->setTextInteractionFlags(Qt::TextSelectableByMouse);
    m_layout->addWidget(m_icon, 0, Qt::AlignTop);
    m_layout->addWidget(m_text, 1);
    updateIcon();
}

void MessageBar::setMessage(Kind kind, const QString &text)
{
    m_text->setText(text);
    if (kind != m_kind) {
        m_kind = kind;
        updateIcon();
        update();
    }
}

void MessageBar::addButton(QAbstractButton *button) { m_layout->addWidget(button, 0, Qt::AlignVCenter); }

void MessageBar::changeEvent(QEvent *event)
{
    QFrame::changeEvent(event);
    // Colors are computed when painting; no style sheet (setting one from
    // here would change the palette again, endlessly).
    if (event->type() == QEvent::PaletteChange || event->type() == QEvent::StyleChange)
        updateIcon();
}

static QColor mix(const QColor &a, const QColor &b, qreal t)
{
    return QColor::fromRgbF(a.redF() + (b.redF() - a.redF()) * t, a.greenF() + (b.greenF() - a.greenF()) * t,
                            a.blueF() + (b.blueF() - a.blueF()) * t);
}

// Breeze's semantic colors; readable mixed into light or dark windows.
QColor MessageBar::accent() const
{
    switch (m_kind) {
    case Positive:
        return {0x27, 0xae, 0x60};
    case Warning:
        return {0xf6, 0x74, 0x00};
    case Error:
        return {0xda, 0x44, 0x53};
    default:
        return {0x3d, 0xae, 0xe9};
    }
}

void MessageBar::updateIcon()
{
    QString name;
    switch (m_kind) {
    case Positive:
        name = QStringLiteral("dialog-positive");
        break;
    case Warning:
        name = QStringLiteral("dialog-warning");
        break;
    case Error:
        name = QStringLiteral("dialog-error");
        break;
    default:
        name = QStringLiteral("dialog-information");
        break;
    }
    const int size = style()->pixelMetric(QStyle::PM_SmallIconSize, nullptr, this);
    m_icon->setPixmap(QIcon::fromTheme(name, QIcon::fromTheme(QStringLiteral("dialog-information"))).pixmap(size, size));
}

void MessageBar::paintEvent(QPaintEvent *)
{
    const QColor window = palette().color(QPalette::Window);
    QPainter p(this);
    p.setRenderHint(QPainter::Antialiasing);
    p.setPen(QPen(mix(window, accent(), 0.55), 1));
    p.setBrush(mix(window, accent(), 0.14));
    p.drawRoundedRect(QRectF(rect()).adjusted(0.5, 0.5, -0.5, -0.5), 6, 6);
}

} // namespace tern
