#include "sound.h"

#include <QDebug>

#include <canberra.h>

namespace tern {

// One context for the process, created on first use; ca_context_play()
// returns immediately and plays in libcanberra's own thread.
static ca_context *context()
{
    static ca_context *ctx = [] {
        ca_context *c = nullptr;
        if (const int r = ca_context_create(&c); r != CA_SUCCESS) {
            qWarning("Sound unavailable: %s", ca_strerror(r));
            return static_cast<ca_context *>(nullptr);
        }
        ca_context_change_props(c, CA_PROP_APPLICATION_NAME, "Tern", CA_PROP_APPLICATION_ID, "fr.karlsen.Tern",
                                CA_PROP_APPLICATION_ICON_NAME, "fr.karlsen.Tern", nullptr);
        return c;
    }();
    return ctx;
}

void playThemeSound(const QString &eventId)
{
    ca_context *ctx = context();
    if (!ctx)
        return;
    const QByteArray id = eventId.toUtf8();
    const int r = ca_context_play(ctx, 0, CA_PROP_EVENT_ID, id.constData(), CA_PROP_EVENT_DESCRIPTION, "New mail",
                                  nullptr);
    if (r != CA_SUCCESS)
        qWarning("Cannot play sound %s: %s", id.constData(), ca_strerror(r));
}

} // namespace tern
