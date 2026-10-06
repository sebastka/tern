#include "core.h"

#include <QMetaObject>

#include <optional>

namespace tern {

namespace {

std::optional<rust::Box<ffi::App>> g_app;
EventBridge *g_bridge = nullptr;

// Implements the Rust → C++ callback interface. Called on Rust runtime
// threads: it only copies data into Qt types and queues a signal emission on
// the bridge's (GUI) thread.
class QtSink final : public ffi::EventSink {
public:
    explicit QtSink(EventBridge *bridge) : m_bridge(bridge) {}

    void folder_tree_changed() const override
    {
        post([](EventBridge *b) { Q_EMIT b->folderTreeChanged(); });
    }
    void list_changed(std::uint32_t count) const override
    {
        post([count](EventBridge *b) { Q_EMIT b->listChanged(count); });
    }
    void message_loaded(ffi::MessageView view) const override
    {
        auto v = std::make_shared<ffi::MessageView>(std::move(view));
        post([v](EventBridge *b) { Q_EMIT b->messageLoaded(v); });
    }
    void compose_ready(ffi::Draft draft) const override
    {
        auto d = std::make_shared<ffi::Draft>(std::move(draft));
        post([d](EventBridge *b) { Q_EMIT b->composeReady(d); });
    }
    void account_status(rust::String account, std::uint8_t state, rust::String text) const override
    {
        post([a = qs(account), s = static_cast<AccountState>(state), t = qs(text)](EventBridge *b) {
            Q_EMIT b->accountStatus(a, s, t);
        });
    }
    void progress(rust::String account, rust::String folder, std::uint32_t done, std::uint32_t total) const override
    {
        post([a = qs(account), f = qs(folder), done, total](EventBridge *b) { Q_EMIT b->progress(a, f, done, total); });
    }
    void config_changed(rust::Vec<rust::String> issues) const override
    {
        post([i = qsl(issues)](EventBridge *b) { Q_EMIT b->configChanged(i); });
    }
    void draft_queued(std::uint64_t request, rust::String error) const override
    {
        post([request, e = qs(error)](EventBridge *b) { Q_EMIT b->draftQueued(request, e); });
    }
    void send_result(bool ok, rust::String text) const override
    {
        post([ok, t = qs(text)](EventBridge *b) { Q_EMIT b->sendResult(ok, t); });
    }
    void error(rust::String text) const override
    {
        post([t = qs(text)](EventBridge *b) { Q_EMIT b->error(t); });
    }
    void raise_window(rust::String token) const override
    {
        post([t = qs(token)](EventBridge *b) { Q_EMIT b->raiseWindow(t); });
    }
    void play_sound(rust::String sound) const override
    {
        post([s = qs(sound)](EventBridge *b) { Q_EMIT b->playSound(s); });
    }
    void show_message(ffi::MessageKey key, std::int64_t folder, rust::String token) const override
    {
        post([k = Key::from(key), folder, t = qs(token)](EventBridge *b) { Q_EMIT b->showMessage(k, folder, t); });
    }

private:
    template<typename F>
    void post(F &&f) const
    {
        // The bridge outlives the core (see main.cpp), and invokeMethod with
        // a context object is thread-safe: the call runs on its thread.
        EventBridge *bridge = m_bridge;
        QMetaObject::invokeMethod(bridge, [bridge, f = std::forward<F>(f)]() { f(bridge); }, Qt::QueuedConnection);
    }

    EventBridge *m_bridge;
};

} // namespace

ffi::App &core()
{
    Q_ASSERT(g_app.has_value());
    return **g_app;
}

EventBridge *bridge() { return g_bridge; }

void Core::init(EventBridge *bridge)
{
    g_bridge = bridge;
    g_app.emplace(ffi::app_new(std::make_unique<QtSink>(bridge)));
}

void Core::shutdown()
{
    // Stops background tasks and the runtime; no events arrive afterwards.
    g_app.reset();
}

QStringList qsl(const rust::Vec<rust::String> &v)
{
    QStringList out;
    out.reserve(static_cast<qsizetype>(v.size()));
    for (const auto &s : v)
        out << qs(s);
    return out;
}

rust::Vec<MessageKey> toFfi(const QList<Key> &keys)
{
    rust::Vec<MessageKey> out;
    out.reserve(static_cast<std::size_t>(keys.size()));
    for (const Key &k : keys)
        out.push_back(k.toFfi());
    return out;
}

} // namespace tern
