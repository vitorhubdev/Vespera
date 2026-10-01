//! Status rows. Expired rows leave on the next read. Message rows are untouched.

use std::path::PathBuf;

use rusqlite::{OptionalExtension, params};

use super::{Archive, Result};
use crate::stories::{Story, StoryKind, TTL_SECS};

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS stories (
    id TEXT PRIMARY KEY,
    sender TEXT NOT NULL,
    sender_name TEXT,
    from_me INTEGER NOT NULL,
    timestamp INTEGER NOT NULL,
    kind TEXT NOT NULL,
    seen INTEGER NOT NULL DEFAULT 0,
    path TEXT,
    raw BLOB
);
CREATE INDEX IF NOT EXISTS stories_by_time ON stories (timestamp);
";

impl Archive {
    pub fn upsert_story(&self, story: &Story, raw: Option<&[u8]>) -> Result<()> {
        let kind = serde_json::to_string(&story.kind).unwrap_or_else(|_| "{}".to_owned());
        self.connection.execute(
            "INSERT INTO stories (id, sender, sender_name, from_me, timestamp, kind, seen, path, raw)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET
                sender = excluded.sender,
                sender_name = COALESCE(excluded.sender_name, sender_name),
                from_me = excluded.from_me,
                timestamp = excluded.timestamp,
                kind = excluded.kind,
                seen = MAX(seen, excluded.seen),
                path = COALESCE(excluded.path, path),
                raw = COALESCE(excluded.raw, raw)",
            params![
                story.id,
                story.sender,
                story.sender_name,
                story.from_me,
                story.timestamp,
                kind,
                story.seen,
                story.path.as_ref().map(|path| path.to_string_lossy().to_string()),
                raw,
            ],
        )?;
        Ok(())
    }

    pub fn stories(&self, now: i64) -> Result<Vec<Story>> {
        self.expire_stories(now)?;
        let mut statement = self.connection.prepare(
            "SELECT id, sender, sender_name, from_me, timestamp, kind, seen, path
             FROM stories ORDER BY timestamp ASC, id ASC",
        )?;
        let rows = statement.query_map([], story_from_row)?;
        rows.collect()
    }

    pub fn story(&self, id: &str) -> Result<Option<Story>> {
        self.connection
            .query_row(
                "SELECT id, sender, sender_name, from_me, timestamp, kind, seen, path
                 FROM stories WHERE id = ?1",
                params![id],
                story_from_row,
            )
            .optional()
    }

    pub fn story_raw(&self, id: &str) -> Result<Option<Vec<u8>>> {
        self.connection
            .query_row(
                "SELECT raw FROM stories WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
    }

    pub fn set_story_seen(&self, id: &str) -> Result<()> {
        self.connection
            .execute("UPDATE stories SET seen = 1 WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn set_story_path(&self, id: &str, path: &std::path::Path) -> Result<()> {
        self.connection.execute(
            "UPDATE stories SET path = ?2 WHERE id = ?1",
            params![id, path.to_string_lossy().to_string()],
        )?;
        Ok(())
    }

    pub fn clear_story_path(&self, id: &str) -> Result<()> {
        self.connection
            .execute("UPDATE stories SET path = NULL WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Removes one status and returns its media path, when it had one.
    pub fn delete_story(&self, id: &str) -> Result<Option<PathBuf>> {
        let path = self.story(id)?.and_then(|story| story.path);
        self.connection
            .execute("DELETE FROM stories WHERE id = ?1", params![id])?;
        Ok(path)
    }

    fn expire_stories(&self, now: i64) -> Result<()> {
        let cutoff = now.saturating_sub(TTL_SECS);
        self.connection
            .execute("DELETE FROM stories WHERE timestamp <= ?1", params![cutoff])?;
        Ok(())
    }
}

fn story_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Story> {
    let kind_json: String = row.get(5)?;
    let kind = serde_json::from_str(&kind_json).unwrap_or(StoryKind::Text {
        text: String::new(),
        background: 0xFF111B21,
        font: 0,
    });
    let path: Option<String> = row.get(7)?;
    Ok(Story {
        id: row.get(0)?,
        sender: row.get(1)?,
        sender_name: row.get(2)?,
        from_me: row.get(3)?,
        timestamp: row.get(4)?,
        kind,
        seen: row.get(6)?,
        path: path.map(PathBuf::from),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stories::{StoryKind, alive};

    fn sample(id: &str, at: i64) -> Story {
        Story {
            id: id.into(),
            sender: "1@s.whatsapp.net".into(),
            sender_name: Some("Ana".into()),
            from_me: false,
            timestamp: at,
            kind: StoryKind::Text {
                text: "hello".into(),
                background: 0xFF1E6E4F,
                font: 0,
            },
            seen: false,
            path: None,
        }
    }

    #[test]
    fn a_status_roundtrips_and_expires_after_a_day() {
        let archive = Archive::in_memory().unwrap();
        let now = 50_000;
        archive
            .upsert_story(&sample("keep", now - 10), Some(b"raw"))
            .unwrap();
        let mut gone = sample("gone", now - TTL_SECS - 5);
        gone.path = Some(PathBuf::from("old.jpg"));
        archive.upsert_story(&gone, None).unwrap();
        let listed = archive.stories(now).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "keep");
        assert!(alive(listed[0].timestamp, now));
        assert_eq!(archive.story_raw("keep").unwrap().unwrap(), b"raw");
        archive.set_story_seen("keep").unwrap();
        assert!(archive.story("keep").unwrap().unwrap().seen);
        archive
            .set_story_path("keep", std::path::Path::new("keep.jpg"))
            .unwrap();
        assert_eq!(
            archive.story("keep").unwrap().unwrap().path.unwrap(),
            PathBuf::from("keep.jpg")
        );
        assert!(archive.story("gone").unwrap().is_none());
    }
}
