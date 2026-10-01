//! Pins and stars for messages. Chat pins live on the chat row and are not stored here.

use rusqlite::{OptionalExtension, params};

use super::Archive;
use crate::model::{ChatPin, Content, FavoriteHit};

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS marks (
    chat TEXT NOT NULL,
    id TEXT NOT NULL,
    starred INTEGER NOT NULL DEFAULT 0,
    pinned_until INTEGER,
    pinned_at INTEGER,
    starred_at INTEGER,
    pin_gen INTEGER,
    PRIMARY KEY (chat, id)
);
";

pub(crate) struct PinRecord {
    pub until: Option<i64>,
    pub at: Option<i64>,
    pub generation: Option<i64>,
}

pub(crate) struct StarRecord {
    pub starred: bool,
    pub at: Option<i64>,
}

impl Archive {
    pub fn set_starred(
        &self,
        chat: &str,
        id: &str,
        starred: bool,
        at: i64,
    ) -> Result<(), rusqlite::Error> {
        self.connection.execute(
            "INSERT INTO marks (chat, id, starred, starred_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(chat, id) DO UPDATE SET starred = excluded.starred, starred_at = excluded.starred_at
             WHERE marks.starred_at IS NULL OR excluded.starred_at >= marks.starred_at",
            params![chat, id, starred as i64, at],
        )?;
        Ok(())
    }

    pub fn replace_star(
        &self,
        chat: &str,
        id: &str,
        starred: bool,
        at: i64,
    ) -> Result<(), rusqlite::Error> {
        self.connection.execute(
            "INSERT INTO marks (chat, id, starred, starred_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(chat, id) DO UPDATE SET starred = excluded.starred, starred_at = excluded.starred_at",
            params![chat, id, i64::from(starred), at],
        )?;
        Ok(())
    }

    pub(crate) fn star_record(
        &self,
        chat: &str,
        id: &str,
    ) -> Result<Option<StarRecord>, rusqlite::Error> {
        self.connection
            .query_row(
                "SELECT starred, starred_at FROM marks WHERE chat = ?1 AND id = ?2",
                params![chat, id],
                |row| {
                    Ok(StarRecord {
                        starred: row.get::<_, i64>(0)? == 1,
                        at: row.get(1)?,
                    })
                },
            )
            .optional()
    }

    pub fn set_pinned_until(
        &self,
        chat: &str,
        id: &str,
        until: Option<i64>,
        at: Option<i64>,
        generation: Option<i64>,
    ) -> Result<(), rusqlite::Error> {
        self.connection.execute(
            "INSERT INTO marks (chat, id, pinned_until, pinned_at, pin_gen) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(chat, id) DO UPDATE SET
                pinned_until = excluded.pinned_until,
                pinned_at = excluded.pinned_at,
                pin_gen = excluded.pin_gen",
            params![chat, id, until, at, generation],
        )?;
        Ok(())
    }

    pub(crate) fn pin_record(
        &self,
        chat: &str,
        id: &str,
    ) -> Result<Option<PinRecord>, rusqlite::Error> {
        self.connection
            .query_row(
                "SELECT pinned_until, pinned_at, pin_gen FROM marks WHERE chat = ?1 AND id = ?2",
                params![chat, id],
                |row| {
                    Ok(PinRecord {
                        until: row.get(0)?,
                        at: row.get(1)?,
                        generation: row.get(2)?,
                    })
                },
            )
            .optional()
    }

    pub fn starred_ids(&self, chat: &str) -> Result<Vec<String>, rusqlite::Error> {
        let mut statement = self
            .connection
            .prepare("SELECT id FROM marks WHERE chat = ?1 AND starred = 1 ORDER BY id")?;
        let rows = statement.query_map(params![chat], |row| row.get(0))?;
        rows.collect()
    }

    pub fn pins(&self, chat: &str, now: i64) -> Result<Vec<ChatPin>, rusqlite::Error> {
        let mut statement = self.connection.prepare(
            "SELECT marks.id, marks.pinned_until, messages.content
             FROM marks
             LEFT JOIN messages ON messages.chat = marks.chat AND messages.id = marks.id
             WHERE marks.chat = ?1 AND marks.pinned_until > ?2
             ORDER BY marks.pinned_until, marks.id",
        )?;
        let rows = statement.query_map(params![chat, now], |row| {
            let content: Option<String> = row.get(2)?;
            let preview = content
                .and_then(|content| serde_json::from_str::<Content>(&content).ok())
                .map(|content| content.summary())
                .unwrap_or_default();
            Ok(ChatPin {
                id: row.get(0)?,
                until: row.get(1)?,
                preview,
            })
        })?;
        rows.collect()
    }

    pub fn favorites(
        &self,
        chat: Option<&str>,
        query: &str,
        limit: usize,
    ) -> Result<Vec<FavoriteHit>, rusqlite::Error> {
        let body = super::search::body_expr("messages.content");
        let mut statement = self.connection.prepare(&format!(
            "SELECT messages.chat, messages.id, messages.timestamp, messages.content
             FROM marks
             JOIN messages ON messages.chat = marks.chat AND messages.id = marks.id
             WHERE marks.starred = 1
               AND (?1 IS NULL OR messages.chat = ?1)
               AND (?2 = '' OR ({body}) LIKE '%' || ?2 || '%' ESCAPE '\\')
             ORDER BY messages.timestamp DESC, messages.id DESC
             LIMIT ?3"
        ))?;
        let needle = like_needle(query);
        let rows = statement.query_map(params![chat, needle, limit as i64], |row| {
            let content: String = row.get(3)?;
            let preview = serde_json::from_str::<Content>(&content)
                .map(|content| content.summary())
                .unwrap_or_default();
            Ok(FavoriteHit {
                chat: row.get(0)?,
                id: row.get(1)?,
                timestamp: row.get(2)?,
                preview,
            })
        })?;
        rows.collect()
    }
}

fn like_needle(query: &str) -> String {
    query
        .trim()
        .chars()
        .flat_map(|ch| match ch {
            '\\' | '%' | '_' => vec!['\\', ch],
            _ => vec![ch],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_star_survives_a_clear_and_a_pin_expires() {
        let archive = Archive::in_memory().unwrap();
        archive.ensure_chat("a@s.whatsapp.net", "A").unwrap();
        let kept = crate::archive::tests::message("a@s.whatsapp.net", "keep", 10, false);
        let gone = crate::archive::tests::message("a@s.whatsapp.net", "gone", 20, false);
        archive.insert_message(&kept, None).unwrap();
        archive.insert_message(&gone, None).unwrap();
        archive
            .set_starred("a@s.whatsapp.net", "keep", true, 1)
            .unwrap();
        archive
            .set_pinned_until("a@s.whatsapp.net", "keep", Some(100), Some(1), None)
            .unwrap();
        archive
            .set_pinned_until("a@s.whatsapp.net", "gone", Some(50), Some(1), None)
            .unwrap();
        assert_eq!(archive.pins("a@s.whatsapp.net", 60).unwrap()[0].id, "keep");
        assert!(archive.pins("a@s.whatsapp.net", 200).unwrap().is_empty());
        archive
            .remove_unstarred_through("a@s.whatsapp.net", 100)
            .unwrap();
        assert!(
            archive
                .message("a@s.whatsapp.net", "keep")
                .unwrap()
                .is_some()
        );
        assert!(
            archive
                .message("a@s.whatsapp.net", "gone")
                .unwrap()
                .is_none()
        );
        let late = crate::archive::tests::message("a@s.whatsapp.net", "late", 30, false);
        archive.insert_starred_history(&late, None).unwrap();
        assert!(
            archive
                .message("a@s.whatsapp.net", "late")
                .unwrap()
                .is_some()
        );
        let hits = archive.favorites(None, "keep", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "keep");
        archive.set_starred("lid@lid", "keep", true, 1).unwrap();
        archive.set_starred("lid@lid", "keep", false, 0).unwrap();
        assert_eq!(
            archive.starred_ids("lid@lid").unwrap(),
            vec!["keep".to_owned()]
        );
        archive
            .ensure_chat("phone@s.whatsapp.net", "Phone")
            .unwrap();
        assert!(
            archive
                .rekey_chat("lid@lid", "phone@s.whatsapp.net")
                .unwrap()
        );
        assert_eq!(
            archive.starred_ids("phone@s.whatsapp.net").unwrap(),
            vec!["keep".to_owned()]
        );
        assert!(archive.starred_ids("lid@lid").unwrap().is_empty());
        archive.clear_chat("a@s.whatsapp.net").unwrap();
        assert!(archive.starred_ids("a@s.whatsapp.net").unwrap().is_empty());
        archive
            .set_pinned_until("lid@lid", "pin", Some(9_999), Some(10), None)
            .unwrap();
        archive
            .set_pinned_until("phone@s.whatsapp.net", "pin", None, Some(20), None)
            .unwrap();
        archive
            .rekey_chat("lid@lid", "phone@s.whatsapp.net")
            .unwrap();
        assert!(archive.pins("phone@s.whatsapp.net", 0).unwrap().is_empty());
    }
}
