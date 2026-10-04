#include "core.h"
#include "mainwindow.h"
#include "messageview.h"
#include "profilepicker.h"

#include <QApplication>
#include <QCommandLineParser>
#include <QDir>
#include <QIcon>
#include <QMessageBox>

#include <algorithm>

using namespace tern;

// Chromium flags for QtWebEngine, merged into QTWEBENGINE_CHROMIUM_FLAGS
// (flags the user set there are kept).
static void setChromiumFlags(const tern::ffi::StartupSettings &settings)
{
    QByteArrayList flags = qgetenv("QTWEBENGINE_CHROMIUM_FLAGS").split(' ');
    flags.removeAll(QByteArray());
    auto has = [&](const QByteArray &name) {
        return std::ranges::any_of(flags, [&](const QByteArray &f) { return f.startsWith(name); });
    };
    // Fedora installs a placeholder Widevine (DRM) plugin that QtWebEngine
    // tries to load at startup ("Unable to load CDM … file too short"). Tern
    // never plays DRM content: point the probe at a path that doesn't exist.
    if (!has("--widevine-path"))
        flags << "--widevine-path=/nonexistent/tern-no-drm";
    // [memory] spare_renderer = false: no renderer process kept in reserve.
    if (!settings.spare_renderer) {
        const QByteArray feature = "SpareRendererForSitePerProcess";
        auto it = std::ranges::find_if(flags, [](const QByteArray &f) { return f.startsWith("--disable-features="); });
        if (it == flags.end())
            flags << "--disable-features=" + feature;
        else if (!it->contains(feature))
            *it += "," + feature;
    }
    qputenv("QTWEBENGINE_CHROMIUM_FLAGS", flags.join(' '));
}

int main(int argc, char *argv[])
{
    // Both must happen before the QApplication exists.
    setChromiumFlags(tern::ffi::startup_settings());
    registerUrlScheme();

    QApplication app(argc, argv);
    QApplication::setApplicationName(QStringLiteral("tern"));
    QApplication::setApplicationDisplayName(QStringLiteral("Tern"));
    QApplication::setApplicationVersion(QStringLiteral(TERN_VERSION));
    QApplication::setDesktopFileName(QStringLiteral("fr.karlsen.Tern"));
    // Installed icon if the theme has it, else the embedded copy.
    QApplication::setWindowIcon(QIcon::fromTheme(QStringLiteral("fr.karlsen.Tern"),
                                                 QIcon(QStringLiteral(":/icons/fr.karlsen.Tern.svg"))));

    QCommandLineParser parser;
    parser.setApplicationDescription(QStringLiteral("Tern mail client"));
    parser.addHelpOption();
    parser.addVersionOption();
    QCommandLineOption profileOpt(QStringList{QStringLiteral("p"), QStringLiteral("profile")},
                                  QStringLiteral("Open <name> without asking."), QStringLiteral("name"));
    parser.addOption(profileOpt);
    parser.process(app);

    // The bridge must outlive the core: core events are queued to it until
    // Core::shutdown() returns.
    EventBridge bridge;
    Core::init(&bridge);
    struct Shutdown {
        ~Shutdown() { Core::shutdown(); }
    } shutdown;

    const auto info = core().startup_info(rs(parser.value(profileOpt)));
    const QStringList profiles = qsl(info.profiles);
    if (!info.issues.empty())
        QMessageBox::warning(nullptr, QStringLiteral("Tern"),
                             QObject::tr("Problems in tern.toml:\n\n%1").arg(qsl(info.issues).join(u'\n')));
    if (profiles.isEmpty()) {
        QMessageBox::information(nullptr, QStringLiteral("Tern"),
                                 QObject::tr("No profile is configured yet.\n\nCreate one in "
                                             "~/.config/tern/profiles/<name>/accounts/<account>.toml "
                                             "(see the documentation) and start Tern again."));
        return 1;
    }

    QString profile = qs(info.auto_profile);
    if (profile.isEmpty()) {
        ProfilePicker picker(profiles, qs(core().last_profile()));
        if (picker.exec() != QDialog::Accepted || picker.selected().isEmpty())
            return 0;
        profile = picker.selected();
    }

    const auto opened = core().open_profile(rs(profile));
    if (!opened.ok) {
        if (opened.already_running) {
            // Single instance per profile: hand over to the running one.
            const QString token = qEnvironmentVariable("XDG_ACTIVATION_TOKEN");
            if (core().raise_existing(rs(profile), rs(token)))
                return 0;
        }
        QMessageBox::critical(nullptr, QStringLiteral("Tern"), qs(opened.message));
        return 1;
    }
    MainWindow window(profile);
    window.show();
    const int rc = app.exec();
    core().close_profile();
    // Attachments opened during this session (see MessageView).
    QDir(openedAttachmentsDir()).removeRecursively();
    return rc;
}
