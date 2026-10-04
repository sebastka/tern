//! Password sources (ARCHITECTURE.md §5).
//!
//! * `password.command = "pass show mail/work"`: run through `sh -c`, the first
//!   line of stdout is the password.
//! * `password.keyring = "..."`: Secret Service lookup by attributes. The value
//!   is either a list of `key=value` pairs separated by whitespace, exactly
//!   like `secret-tool lookup` takes them (`"service=mail account=work"`), or
//!   a single word `entry`, which is shorthand for `"service=tern entry=<entry>"`.

use std::collections::HashMap;

use secret_service::{EncryptionType, SecretService};
use tokio::process::Command;

use crate::model::PasswordSource;

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("password command failed: {0}")]
    Command(String),
    #[error("secret service: {0}")]
    Keyring(String),
    #[error("no secret found for keyring entry {0:?}")]
    NotFound(String),
    #[error("invalid password source")]
    Invalid,
}

/// Parse a `password.keyring` value into Secret Service attributes.
pub fn keyring_attributes(spec: &str) -> HashMap<String, String> {
    let spec = spec.trim();
    if spec.contains('=') {
        spec.split_whitespace().filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_owned(), v.to_owned())).collect()
    } else {
        HashMap::from([("service".into(), "tern".into()), ("entry".into(), spec.into())])
    }
}

/// Resolve a password. May prompt (pinentry from `pass`, keyring unlock).
pub async fn resolve(source: &PasswordSource) -> Result<String, SecretError> {
    match (&source.command, &source.keyring) {
        (Some(cmd), None) => run_command(cmd).await,
        (None, Some(spec)) => keyring_lookup(spec).await,
        _ => Err(SecretError::Invalid),
    }
}

async fn run_command(cmd: &str) -> Result<String, SecretError> {
    let out = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| SecretError::Command(e.to_string()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(SecretError::Command(format!(
            "`{cmd}` exited with {}: {}",
            out.status,
            err.lines().next().unwrap_or("")
        )));
    }
    let stdout = String::from_utf8(out.stdout).map_err(|_| SecretError::Command("output is not UTF-8".into()))?;
    let line = stdout.lines().next().unwrap_or("").to_owned();
    if line.is_empty() {
        return Err(SecretError::Command(format!("`{cmd}` printed an empty first line")));
    }
    Ok(line)
}

async fn keyring_lookup(spec: &str) -> Result<String, SecretError> {
    let err = |e: secret_service::Error| SecretError::Keyring(e.to_string());
    let attrs = keyring_attributes(spec);
    let attrs_ref: HashMap<&str, &str> = attrs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let ss = SecretService::connect(EncryptionType::Dh).await.map_err(err)?;
    let found = ss.search_items(attrs_ref).await.map_err(err)?;
    let item = match found.unlocked.into_iter().next() {
        Some(i) => i,
        None => {
            let item = found.locked.into_iter().next().ok_or_else(|| SecretError::NotFound(spec.to_owned()))?;
            item.unlock().await.map_err(err)?;
            item
        }
    };
    let secret = item.get_secret().await.map_err(err)?;
    String::from_utf8(secret).map_err(|_| SecretError::Keyring("secret is not UTF-8".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attributes() {
        let a = keyring_attributes("service=mail  account=work");
        assert_eq!(a.len(), 2);
        assert_eq!(a["account"], "work");
        let b = keyring_attributes("posteo");
        assert_eq!(b["service"], "tern");
        assert_eq!(b["entry"], "posteo");
    }

    #[tokio::test]
    async fn command_first_line() {
        let src = PasswordSource { command: Some("printf 'pw1\\nother\\n'".into()), keyring: None };
        assert_eq!(resolve(&src).await.unwrap(), "pw1");
        let src = PasswordSource { command: Some("exit 3".into()), keyring: None };
        assert!(resolve(&src).await.is_err());
    }
}
