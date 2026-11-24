//! Saved connections, query history and passwords.
//!
//! Files live in `~/Library/Application Support/OurDB` (or `$OURDB_CONFIG_DIR`).
//! Passwords go to the macOS Keychain (service "OurDB", account = connection id).
//! With `OURDB_CONFIG_DIR` set, passwords stay in memory only, so demo runs
//! never touch the real Keychain.

use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use ourdb_core::ConnConfig;
use serde::{Deserialize, Serialize};

const KEYCHAIN_SERVICE: &str = "OurDB";
const HISTORY_MAX: usize = 100;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub text: String,
    /// Seconds since the Unix epoch.
    pub at: u64,
}

pub struct Store {
    dir: PathBuf,
    /// True when passwords must not go to the Keychain.
    memory_only: bool,
    /// Passwords for this session only (memory-only mode or a Keychain failure).
    memory: HashMap<String, String>,
    pub connections: Vec<ConnConfig>,
    pub history: HashMap<String, Vec<HistoryEntry>>,
}

impl Store {
    pub fn load() -> Self {
        let custom = std::env::var_os("OURDB_CONFIG_DIR").filter(|v| !v.is_empty());
        let memory_only = custom.is_some();
        let dir = match custom {
            Some(d) => PathBuf::from(d),
            None => dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("OurDB"),
        };
        let connections = read_json(&dir.join("connections.json")).unwrap_or_default();
        let history = read_json(&dir.join("history.json")).unwrap_or_default();
        Self {
            dir,
            memory_only,
            memory: HashMap::new(),
            connections,
            history,
        }
    }

    pub fn save_connections(&self) -> Result<(), String> {
        write_json(&self.dir, "connections.json", &self.connections)
    }

    pub fn get(&self, id: &str) -> Option<&ConnConfig> {
        self.connections.iter().find(|c| c.id == id)
    }

    /// Add a new connection or replace the one with the same id.
    pub fn upsert(&mut self, cfg: ConnConfig) {
        match self.connections.iter_mut().find(|c| c.id == cfg.id) {
            Some(c) => *c = cfg,
            None => self.connections.push(cfg),
        }
    }

    pub fn remove(&mut self, id: &str) {
        self.connections.retain(|c| c.id != id);
        self.history.remove(id);
        self.memory.remove(id);
        if !self.memory_only
            && let Ok(e) = keyring::Entry::new(KEYCHAIN_SERVICE, id)
        {
            let _ = e.delete_credential();
        }
    }

    /// The saved password, or "" when there is none.
    pub fn password(&self, id: &str) -> Result<String, String> {
        if let Some(p) = self.memory.get(id) {
            return Ok(p.clone());
        }
        if self.memory_only {
            return Ok(String::new());
        }
        let entry =
            keyring::Entry::new(KEYCHAIN_SERVICE, id).map_err(|e| format!("Keychain: {e}"))?;
        match entry.get_password() {
            Ok(p) => Ok(p),
            Err(keyring::Error::NoEntry) => Ok(String::new()),
            Err(e) => Err(format!("Keychain: {e}")),
        }
    }

    /// Save a password. An empty password deletes the Keychain item.
    /// On a Keychain error the password is kept for this session only.
    pub fn set_password(&mut self, id: &str, password: &str) -> Result<(), String> {
        if self.memory_only {
            self.memory.insert(id.to_string(), password.to_string());
            return Ok(());
        }
        let result = keyring::Entry::new(KEYCHAIN_SERVICE, id).and_then(|e| {
            if password.is_empty() {
                match e.delete_credential() {
                    Err(keyring::Error::NoEntry) => Ok(()),
                    other => other,
                }
            } else {
                e.set_password(password)
            }
        });
        match result {
            Ok(()) => {
                self.memory.remove(id);
                Ok(())
            }
            Err(e) => {
                self.memory.insert(id.to_string(), password.to_string());
                Err(format!(
                    "Keychain: {e}. The password is kept for this session only."
                ))
            }
        }
    }

    pub fn history_of(&self, conn_id: &str) -> &[HistoryEntry] {
        self.history.get(conn_id).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Put a query at the top of the history of a connection and save the file.
    pub fn add_history(&mut self, conn_id: &str, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let list = self.history.entry(conn_id.to_string()).or_default();
        list.retain(|h| h.text != text);
        list.insert(
            0,
            HistoryEntry {
                text: text.to_string(),
                at,
            },
        );
        list.truncate(HISTORY_MAX);
        let _ = write_json(&self.dir, "history.json", &self.history);
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &PathBuf) -> Option<T> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Write to a temp file first, then rename, so a crash never leaves half a file.
fn write_json<T: Serialize>(dir: &PathBuf, name: &str, value: &T) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(".{name}.tmp"));
    fs::write(&tmp, text).map_err(|e| format!("Cannot write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, dir.join(name)).map_err(|e| format!("Cannot save {name}: {e}"))
}
