// Startup profile picker (§6).
#pragma once

#include <QDialog>

class QListWidget;

namespace tern {

class ProfilePicker : public QDialog {
    Q_OBJECT
public:
    ProfilePicker(const QStringList &profiles, const QString &preselect, QWidget *parent = nullptr);
    QString selected() const;

private:
    QListWidget *m_list = nullptr;
};

} // namespace tern
