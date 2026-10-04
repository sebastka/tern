//! Single instance per profile (ARCHITECTURE.md §6): the process owning a
//! profile exports `Raise` on the session bus; a second `tern` for the same
//! profile calls it and exits. The XDG activation token is passed along so
//! Wayland compositors let the existing window take focus.

use std::sync::Arc;

use crate::types::Event;

pub const OBJECT_PATH: &str = "/fr/karlsen/Tern";
pub const INTERFACE: &str = "fr.karlsen.Tern1";

/// Well-known bus name for a profile: `fr.karlsen.Tern.p_<profile>` with
/// characters outside `[A-Za-z0-9_]` replaced.
pub fn bus_name(profile: &str) -> String {
    let safe: String = profile.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
    format!("fr.karlsen.Tern.p_{safe}")
}

struct Raiser {
    sink: Arc<dyn Fn(Event) + Send + Sync>,
}

#[zbus::interface(name = "fr.karlsen.Tern1")]
impl Raiser {
    fn raise(&self, activation_token: String) {
        tracing::info!("another instance asked to raise the window");
        (self.sink)(Event::RaiseWindow { activation_token });
    }
}

/// Claim the profile's bus name and serve `Raise`. Keep the connection alive.
pub async fn serve(profile: &str, sink: Arc<dyn Fn(Event) + Send + Sync>) -> zbus::Result<zbus::Connection> {
    zbus::connection::Builder::session()?.name(bus_name(profile))?.serve_at(OBJECT_PATH, Raiser { sink })?.build().await
}

/// Ask the instance owning `profile` to raise its window.
pub async fn raise_existing(profile: &str, activation_token: &str) -> zbus::Result<()> {
    let conn = zbus::Connection::session().await?;
    conn.call_method(Some(bus_name(profile).as_str()), OBJECT_PATH, Some(INTERFACE), "Raise", &(activation_token,))
        .await?;
    Ok(())
}
