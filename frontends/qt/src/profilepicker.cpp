#include "profilepicker.h"

#include <QDialogButtonBox>
#include <QLabel>
#include <QListWidget>
#include <QPushButton>
#include <QVBoxLayout>

namespace tern {

ProfilePicker::ProfilePicker(const QStringList &profiles, const QString &preselect, QWidget *parent)
    : QDialog(parent)
{
    setWindowTitle(tr("Tern — Choose Profile"));
    m_list = new QListWidget(this);
    for (const QString &p : profiles) {
        auto *item = new QListWidgetItem(QIcon::fromTheme(QStringLiteral("user-identity")), p, m_list);
        if (p == preselect)
            m_list->setCurrentItem(item);
    }
    if (!m_list->currentItem() && m_list->count() > 0)
        m_list->setCurrentRow(0);

    auto *buttons = new QDialogButtonBox(QDialogButtonBox::Open | QDialogButtonBox::Cancel, this);
    connect(buttons, &QDialogButtonBox::accepted, this, &QDialog::accept);
    connect(buttons, &QDialogButtonBox::rejected, this, &QDialog::reject);
    connect(m_list, &QListWidget::itemActivated, this, &QDialog::accept);

    auto *layout = new QVBoxLayout(this);
    layout->addWidget(new QLabel(tr("Open which profile?"), this));
    layout->addWidget(m_list);
    layout->addWidget(buttons);
    resize(360, 300);
}

QString ProfilePicker::selected() const
{
    return m_list->currentItem() ? m_list->currentItem()->text() : QString();
}

} // namespace tern
