// Event callbacks from the Rust core to the C++ frontend.
//
// Methods are called on Rust runtime threads, never on the GUI thread.
// Implementations must be thread-safe and only hand the data over to the GUI
// thread (QMetaObject::invokeMethod with Qt::QueuedConnection).
#pragma once

#include "rust/cxx.h"

#include <cstdint>

namespace tern::ffi {

struct MessageView;
struct MessageKey;
struct Draft;

class EventSink {
public:
    virtual ~EventSink() = default;

    virtual void folder_tree_changed() const = 0;
    virtual void list_changed(std::uint32_t count) const = 0;
    virtual void message_loaded(MessageView view) const = 0;
    virtual void compose_ready(Draft draft) const = 0;
    // state: see AccountState in the generated bridge header.
    virtual void account_status(rust::String account, std::uint8_t state, rust::String text) const = 0;
    virtual void progress(rust::String account, rust::String folder, std::uint32_t done, std::uint32_t total) const = 0;
    virtual void config_changed(rust::Vec<rust::String> issues) const = 0;
    virtual void draft_queued(std::uint64_t request, rust::String error) const = 0;
    virtual void send_result(bool ok, rust::String text) const = 0;
    virtual void error(rust::String text) const = 0;
    virtual void raise_window(rust::String activation_token) const = 0;
    // A sender picture was found for `email` (PNG/JPEG/GIF bytes).
    virtual void avatar_ready(rust::String email, rust::Vec<std::uint8_t> image) const = 0;
    // A freedesktop sound theme event id, e.g. "message-new-email".
    virtual void play_sound(rust::String sound) const = 0;
    // A new-mail notification was clicked: raise and show this message.
    virtual void show_message(MessageKey key, std::int64_t folder, rust::String activation_token) const = 0;
};

} // namespace tern::ffi
