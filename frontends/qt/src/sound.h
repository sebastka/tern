// Event sounds from the freedesktop sound theme, through libcanberra (works
// with PipeWire/PulseAudio on any desktop).
#pragma once

#include <QString>

namespace tern {

// Play a sound theme event, e.g. "message-new-email". Missing names fall
// back as the sound theme spec defines ("message-new" → "message"). Never
// blocks; failures are only logged.
void playThemeSound(const QString &eventId);

} // namespace tern
