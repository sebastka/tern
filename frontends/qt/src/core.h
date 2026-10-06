// Access to the Rust core and the event bridge to the GUI thread.
#pragma once

#include "tern-ffi/src/lib.rs.h"

#include <QList>
#include <QObject>
#include <QString>
#include <QStringList>

#include <memory>

namespace tern {

class EventBridge;

using ffi::MessageKey;

// The Rust application facade. Valid between Core::init() and Core::shutdown().
ffi::App &core();

namespace Core {
// Create the Rust app; events are delivered through `bridge`.
void init(EventBridge *bridge);
void shutdown();
} // namespace Core

inline QString qs(const rust::String &s) { return QString::fromUtf8(s.data(), static_cast<qsizetype>(s.size())); }
inline QString qs(rust::Str s) { return QString::fromUtf8(s.data(), static_cast<qsizetype>(s.size())); }
inline rust::String rs(const QString &s)
{
    const QByteArray utf8 = s.toUtf8();
    return rust::String(utf8.constData(), static_cast<std::size_t>(utf8.size()));
}
QStringList qsl(const rust::Vec<rust::String> &v);

// Keys are cheap to copy and are used as values in the UI.
struct Key {
    QString account;
    qint64 id = 0;
    bool valid() const { return !account.isEmpty(); }
    bool operator==(const Key &) const = default;
    MessageKey toFfi() const { return MessageKey{rs(account), id}; }
    static Key from(const MessageKey &k) { return Key{qs(k.account), k.id}; }
};

rust::Vec<MessageKey> toFfi(const QList<Key> &keys);
inline rust::Slice<const MessageKey> slice(const rust::Vec<MessageKey> &v) { return {v.data(), v.size()}; }

// The application's event bridge (owned by main()).
EventBridge *bridge();

enum class AccountState : quint8 { Connecting, Syncing, Online, Offline, Error };

// Receives core events on Rust threads and re-emits them as Qt signals on
// the GUI thread (queued). Never touches widgets itself.
class EventBridge : public QObject {
    Q_OBJECT
public:
    using QObject::QObject;

Q_SIGNALS:
    void folderTreeChanged();
    void listChanged(quint32 count);
    void messageLoaded(std::shared_ptr<tern::ffi::MessageView> view);
    void composeReady(std::shared_ptr<tern::ffi::Draft> draft);
    void accountStatus(const QString &account, tern::AccountState state, const QString &text);
    void progress(const QString &account, const QString &folder, quint32 done, quint32 total);
    void configChanged(const QStringList &issues);
    void draftQueued(quint64 request, const QString &error);
    void sendResult(bool ok, const QString &text);
    void error(const QString &text);
    void raiseWindow(const QString &activationToken);
    // A freedesktop sound theme event id.
    void playSound(const QString &sound);
    // A new-mail notification was clicked.
    void showMessage(const tern::Key &key, qint64 folder, const QString &activationToken);
};

} // namespace tern
