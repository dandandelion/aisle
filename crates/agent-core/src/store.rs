//! SQLite session store. Schema is spec §9.2 verbatim.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use zbus::zvariant::Type;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    /// empty string = use provider default (zvariant structs can't hold Option)
    pub model: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct Msg {
    pub id: i64,
    pub session_id: String,
    pub role: String,
    pub text: String,
    pub created_at: String,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &std::path::Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions(
                 id TEXT PRIMARY KEY,
                 title TEXT NOT NULL DEFAULT '',
                 model TEXT,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS messages(
                 id INTEGER PRIMARY KEY,
                 session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                 role TEXT NOT NULL CHECK(role IN ('user','assistant','system')),
                 text TEXT NOT NULL,
                 created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS attachments(
                 id INTEGER PRIMARY KEY,
                 message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
                 kind TEXT NOT NULL DEFAULT 'screenshot',
                 path TEXT NOT NULL,
                 width INTEGER NOT NULL,
                 height INTEGER NOT NULL,
                 bytes INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id);",
        )?;
        Ok(Self { conn })
    }

    fn now() -> String {
        chrono_now()
    }

    pub fn create_session(&mut self, title: &str, model: Option<&str>) -> Result<SessionInfo> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = Self::now();
        self.conn.execute(
            "INSERT INTO sessions(id, title, model, created_at, updated_at) VALUES(?,?,?,?,?)",
            params![id, title, model, now, now],
        )?;
        Ok(SessionInfo { id, title: title.into(), model: model.unwrap_or_default().into(), created_at: now.clone(), updated_at: now })
    }

    pub fn get_session(&self, id: &str) -> Result<SessionInfo> {
        self.conn
            .query_row(
                "SELECT id, title, model, created_at, updated_at FROM sessions WHERE id=?1",
                params![id],
                |r| {
                    Ok(SessionInfo {
                        id: r.get(0)?,
                        title: r.get(1)?,
                        model: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        created_at: r.get(3)?,
                        updated_at: r.get(4)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| Error::UnknownSession(id.into()))
    }

    pub fn latest_session(&self) -> Result<Option<SessionInfo>> {
        self.conn
            .query_row(
                "SELECT id, title, model, created_at, updated_at FROM sessions ORDER BY updated_at DESC LIMIT 1",
                [],
                |r| {
                    Ok(SessionInfo {
                        id: r.get(0)?,
                        title: r.get(1)?,
                        model: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        created_at: r.get(3)?,
                        updated_at: r.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, model, created_at, updated_at FROM sessions ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(SessionInfo {
                id: r.get(0)?,
                title: r.get(1)?,
                model: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                created_at: r.get(3)?,
                updated_at: r.get(4)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    pub fn set_model(&mut self, session_id: &str, model: Option<&str>) -> Result<()> {
        let n = self.conn.execute("UPDATE sessions SET model=?2 WHERE id=?1", params![session_id, model])?;
        if n == 0 {
            return Err(Error::UnknownSession(session_id.into()));
        }
        Ok(())
    }

    pub fn add_message(&mut self, session_id: &str, role: &str, text: &str) -> Result<Msg> {
        let now = Self::now();
        self.conn.execute(
            "INSERT INTO messages(session_id, role, text, created_at) VALUES(?,?,?,?)",
            params![session_id, role, text, now],
        )?;
        self.conn.execute(
            "UPDATE sessions SET updated_at=?2 WHERE id=?1",
            params![session_id, now],
        )?;
        // v1 title rule: first user message, truncated (spec §13.4).
        let title: String = self.conn.query_row(
            "SELECT title FROM sessions WHERE id=?1",
            params![session_id],
            |r| r.get(0),
        )?;
        if title.is_empty() && role == "user" {
            let t: String = text.chars().take(60).collect();
            self.conn.execute("UPDATE sessions SET title=?2 WHERE id=?1", params![session_id, t])?;
        }
        Ok(Msg { id: self.conn.last_insert_rowid(), session_id: session_id.into(), role: role.into(), text: text.into(), created_at: now })
    }

    pub fn get_messages(&self, session_id: &str, tail: usize) -> Result<Vec<Msg>> {
        let mut all: Vec<Msg> = {
            let mut stmt = self.conn.prepare(
                "SELECT id, session_id, role, text, created_at FROM messages WHERE session_id=?1 ORDER BY id",
            )?;
            let rows = stmt.query_map(params![session_id], |r| {
                Ok(Msg { id: r.get(0)?, session_id: r.get(1)?, role: r.get(2)?, text: r.get(3)?, created_at: r.get(4)? })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let skip = all.len().saturating_sub(tail);
        Ok(all.split_off(skip))
    }

    pub fn delete_session(&mut self, session_id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM sessions WHERE id=?1", params![session_id])?;
        Ok(())
    }

    pub fn all_attachment_paths(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT path FROM attachments")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    pub fn add_attachment(
        &mut self,
        message_id: i64,
        path: &str,
        width: i64,
        height: i64,
        bytes: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO attachments(message_id, kind, path, width, height, bytes) VALUES(?1,'screenshot',?2,?3,?4,?5)",
            params![message_id, path, width, height, bytes],
        )?;
        Ok(())
    }

    /// (message_id, path, width, height, bytes) for all attachments in a session.
    pub fn attachments_for(&self, session_id: &str) -> Result<Vec<(i64, String, i64, i64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT a.message_id, a.path, a.width, a.height, a.bytes
             FROM attachments a JOIN messages m ON m.id = a.message_id
             WHERE m.session_id = ?1 ORDER BY a.message_id",
        )?;
        let rows = stmt.query_map(params![session_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }
}

fn chrono_now() -> String {
    // RFC 3339 UTC without pulling in chrono for now.
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    // crude but sortable fixed-width format is enough for v1 ordering
    format!("{secs}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(tag: &str) -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("sideagent-store-test-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (Store::open(&dir.join("s.db")).unwrap(), dir)
    }

    #[test]
    fn session_and_messages_roundtrip() {
        let (mut store, dir) = temp_store("rt");
        let s = store.create_session("", None).unwrap();
        store.add_message(&s.id, "user", "first question here").unwrap();
        store.add_message(&s.id, "assistant", "answer").unwrap();
        let msgs = store.get_messages(&s.id, 100).unwrap();
        assert_eq!(msgs.len(), 2);
        // title auto-set from first user message
        let s2 = store.get_session(&s.id).unwrap();
        assert_eq!(s2.title, "first question here");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn tail_limits_messages() {
        let (mut store, dir) = temp_store("tail");
        let s = store.create_session("t", None).unwrap();
        for i in 0..10 {
            store.add_message(&s.id, "user", &format!("m{i}")).unwrap();
        }
        let msgs = store.get_messages(&s.id, 3).unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].text, "m7");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn reopen_persists() {
        let (mut store, dir) = temp_store("reopen");
        let s = store.create_session("x", None).unwrap();
        store.add_message(&s.id, "user", "hello").unwrap();
        drop(store);
        let store = Store::open(&dir.join("s.db")).unwrap();
        assert_eq!(store.get_messages(&s.id, 100).unwrap().len(), 1);
        std::fs::remove_dir_all(dir).ok();
    }
}
