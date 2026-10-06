//! New-mail desktop notifications through the freedesktop Desktop
//! Notifications spec (`org.freedesktop.Notifications` on the session bus),
//! so they work with any notification server: Plasma, GNOME, mako, dunst...
//!
//! The sound is not left to the server (many don't play one): notifications
//! carry `suppress-sound`, and the frontend plays the sound theme's
//! `message-new-email` itself (`Event::PlaySound`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use futures::StreamExt;
use tracing::{debug, warn};
use zbus::zvariant::Value;

use crate::hub::Sink;
use crate::types::{Event, FolderKey, MessageKey};

/// Sound theme event for new mail (freedesktop sound naming spec).
pub const NEW_MAIL_SOUND: &str = "message-new-email";

const DESTINATION: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
const INTERFACE: &str = "org.freedesktop.Notifications";
/// Installed icon and desktop file name.
const APP_ID: &str = "fr.karlsen.Tern";
/// Lines listed in a summary notification for several messages.
const MAX_LINES: usize = 4;
/// Clicked-notification targets kept; servers that never report closing
/// must not make this grow forever.
const MAX_TARGETS: usize = 200;

/// One new message, for the notification text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arrival {
    pub sender: String,
    pub subject: String,
}

/// Summary and body lines for an account's new messages (newest first).
/// `account` is shown when the profile has several accounts.
pub fn compose(arrivals: &[Arrival], account: Option<&str>) -> (String, Vec<String>) {
    let subject = |a: &Arrival| if a.subject.trim().is_empty() { "(no subject)".to_owned() } else { a.subject.clone() };
    match arrivals {
        [] => (String::new(), Vec::new()),
        [one] => {
            let summary = match account {
                Some(acc) => format!("{} ({acc})", one.sender),
                None => one.sender.clone(),
            };
            (summary, vec![subject(one)])
        }
        many => {
            let summary = match account {
                Some(acc) => format!("{} new messages ({acc})", many.len()),
                None => format!("{} new messages", many.len()),
            };
            let mut lines: Vec<String> =
                many.iter().take(MAX_LINES).map(|a| format!("{}: {}", a.sender, subject(a))).collect();
            if many.len() > MAX_LINES {
                lines.push(format!("… and {} more", many.len() - MAX_LINES));
            }
            (summary, lines)
        }
    }
}

/// Escape text for servers that interpret body markup.
fn escape_markup(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub struct Notifier {
    conn: zbus::Connection,
    /// The server supports actions (clicking opens the message).
    actions: bool,
    /// The server interprets markup in the body: text must be escaped.
    markup: bool,
    /// Notification id → message to show when it's clicked.
    targets: Mutex<HashMap<u32, (MessageKey, FolderKey)>>,
    /// Notification id → XDG activation token (arrives before the click).
    tokens: Mutex<HashMap<u32, String>>,
    listener: OnceLock<tokio::task::JoinHandle<()>>,
}

impl Drop for Notifier {
    fn drop(&mut self) {
        if let Some(t) = self.listener.get() {
            t.abort();
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Notifier {
    /// Connect to the notification server and listen for clicks. Fails when
    /// there is no session bus or no notification server.
    pub async fn connect(sink: Sink) -> zbus::Result<Arc<Self>> {
        let conn = zbus::Connection::session().await?;
        let caps: Vec<String> = conn
            .call_method(Some(DESTINATION), PATH, Some(INTERFACE), "GetCapabilities", &())
            .await?
            .body()
            .deserialize()?;
        debug!(?caps, "notification server capabilities");
        let rule =
            zbus::MatchRule::builder().msg_type(zbus::message::Type::Signal).interface(INTERFACE)?.path(PATH)?.build();
        // One stream for all signals keeps their order: ActivationToken is
        // sent right before ActionInvoked.
        let mut signals = zbus::MessageStream::for_match_rule(rule, &conn, None).await?;
        let notifier = Arc::new(Self {
            conn,
            actions: caps.iter().any(|c| c == "actions"),
            markup: caps.iter().any(|c| c == "body-markup"),
            targets: Mutex::new(HashMap::new()),
            tokens: Mutex::new(HashMap::new()),
            listener: OnceLock::new(),
        });
        let weak: Weak<Self> = Arc::downgrade(&notifier);
        let task = tokio::spawn(async move {
            while let Some(Ok(msg)) = signals.next().await {
                let Some(n) = weak.upgrade() else { return };
                let header = msg.header();
                let Some(member) = header.member() else { continue };
                match member.as_str() {
                    "ActivationToken" => {
                        if let Ok((id, token)) = msg.body().deserialize::<(u32, String)>()
                            && lock(&n.targets).contains_key(&id)
                        {
                            lock(&n.tokens).insert(id, token);
                        }
                    }
                    "ActionInvoked" => {
                        let Ok((id, action)) = msg.body().deserialize::<(u32, String)>() else { continue };
                        if action != "default" {
                            continue;
                        }
                        let Some((key, folder)) = lock(&n.targets).remove(&id) else { continue };
                        let activation_token = lock(&n.tokens).remove(&id).unwrap_or_default();
                        sink(Event::ShowMessage { key, folder, activation_token });
                    }
                    "NotificationClosed" => {
                        if let Ok((id, _reason)) = msg.body().deserialize::<(u32, u32)>() {
                            lock(&n.targets).remove(&id);
                            lock(&n.tokens).remove(&id);
                        }
                    }
                    _ => {}
                }
            }
        });
        let _ = notifier.listener.set(task);
        Ok(notifier)
    }

    /// Show a notification; clicking it shows `target`.
    pub async fn show(&self, summary: &str, lines: &[String], target: (MessageKey, FolderKey)) {
        let body = lines.join("\n");
        let body = if self.markup { escape_markup(&body) } else { body };
        let actions: &[&str] = if self.actions { &["default", "Open"] } else { &[] };
        let hints: HashMap<&str, Value<'_>> = HashMap::from([
            ("desktop-entry", Value::from(APP_ID)),
            ("category", Value::from("email.arrived")),
            ("urgency", Value::from(1u8)),
            ("suppress-sound", Value::from(true)),
        ]);
        let reply = self
            .conn
            .call_method(
                Some(DESTINATION),
                PATH,
                Some(INTERFACE),
                "Notify",
                &("Tern", 0u32, APP_ID, summary, body.as_str(), actions, hints, -1i32),
            )
            .await;
        match reply.and_then(|r| r.body().deserialize::<u32>()) {
            Ok(id) if self.actions => {
                let mut targets = lock(&self.targets);
                if targets.len() >= MAX_TARGETS {
                    targets.clear();
                    lock(&self.tokens).clear();
                }
                targets.insert(id, target);
            }
            Ok(_) => {}
            Err(e) => warn!("cannot show notification: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(sender: &str, subject: &str) -> Arrival {
        Arrival { sender: sender.into(), subject: subject.into() }
    }

    #[test]
    fn single_and_summary() {
        assert_eq!(compose(&[a("Ann", "Hi")], None), ("Ann".into(), vec!["Hi".into()]));
        assert_eq!(compose(&[a("Ann", " ")], Some("Work")), ("Ann (Work)".into(), vec!["(no subject)".into()]));
        let many: Vec<Arrival> = (1..=6).map(|i| a(&format!("S{i}"), &format!("T{i}"))).collect();
        let (summary, lines) = compose(&many, None);
        assert_eq!(summary, "6 new messages");
        assert_eq!(lines, ["S1: T1", "S2: T2", "S3: T3", "S4: T4", "… and 2 more"]);
        assert_eq!(compose(&many[..2], Some("Posteo")).0, "2 new messages (Posteo)");
    }

    #[test]
    fn markup_escaping() {
        assert_eq!(escape_markup("<b>R&D</b>"), "&lt;b&gt;R&amp;D&lt;/b&gt;");
    }
}
