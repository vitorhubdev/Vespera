//! Full-text index for message search.
//!
//! The searchable text (body, caption, file name, poll question, contact
//! name, and the interactive body, footer, and title) lives in
//! `message_search`. An FTS5 external-content table indexes it with the
//! trigram tokenizer, so a query is a substring and ASCII case and
//! diacritics are folded. Triggers keep the index aligned with `messages`.
//! Rows that already existed when the index was added are copied in batches.

use rusqlite::{Connection, OptionalExtension, params};

use crate::model::{Content, Message};

use super::status_from_rank;

const FIELDS: &[&str] = &[
    "$.text",
    "$.caption",
    "$.file_name",
    "$.question",
    "$.display_name",
    "$.name",
    "$.body",
    "$.footer",
    "$.header.text",
    "$.options",
];

/// Trigram queries shorter than this match nothing, so they stay on the scan.
pub(super) const MIN_TRIGRAM: usize = 3;

pub(super) fn body_expr(column: &str) -> String {
    FIELDS
        .iter()
        .map(|field| format!("coalesce(json_extract({column}, '{field}'), '')"))
        .collect::<Vec<_>>()
        .join(" || char(10) || ")
}

/// Creates the index and the triggers. Safe to run on every open.
pub(super) fn install(connection: &Connection) -> rusqlite::Result<()> {
    let inserted = body_expr("new.content");
    let updated = body_expr("new.content");
    connection.execute_batch(&format!(
        "
        CREATE TABLE IF NOT EXISTS message_search (
            rowid INTEGER PRIMARY KEY,
            body TEXT NOT NULL
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS message_fts USING fts5(
            body,
            content='message_search',
            content_rowid='rowid',
            tokenize = 'trigram remove_diacritics 2'
        );
        CREATE TRIGGER IF NOT EXISTS messages_search_ai AFTER INSERT ON messages BEGIN
            INSERT INTO message_search(rowid, body) VALUES (new.rowid, {inserted});
        END;
        CREATE TRIGGER IF NOT EXISTS messages_search_au AFTER UPDATE OF content ON messages BEGIN
            INSERT INTO message_search(rowid, body) VALUES (new.rowid, {updated})
            ON CONFLICT(rowid) DO UPDATE SET body = excluded.body;
        END;
        CREATE TRIGGER IF NOT EXISTS messages_search_ad AFTER DELETE ON messages BEGIN
            DELETE FROM message_search WHERE rowid = old.rowid;
        END;
        CREATE TRIGGER IF NOT EXISTS message_search_ai AFTER INSERT ON message_search BEGIN
            INSERT INTO message_fts(rowid, body) VALUES (new.rowid, new.body);
        END;
        CREATE TRIGGER IF NOT EXISTS message_search_ad AFTER DELETE ON message_search BEGIN
            INSERT INTO message_fts(message_fts, rowid, body) VALUES('delete', old.rowid, old.body);
        END;
        CREATE TRIGGER IF NOT EXISTS message_search_au AFTER UPDATE OF body ON message_search BEGIN
            INSERT INTO message_fts(message_fts, rowid, body) VALUES('delete', old.rowid, old.body);
            INSERT INTO message_fts(rowid, body) VALUES (new.rowid, new.body);
        END;
        "
    ))?;
    Ok(())
}

/// Whether the index covers every stored message.
pub(super) fn ready(connection: &Connection) -> rusqlite::Result<bool> {
    let value: Option<String> = connection
        .query_row(
            "SELECT value FROM meta WHERE key = 'search_index'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value.as_deref() == Some("ready"))
}

pub(super) fn mark_ready(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO meta (key, value) VALUES ('search_index', 'ready')
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [],
    )?;
    Ok(())
}

/// Indexed rows and total messages. The second number is the backlog's end.
pub(super) fn backlog(connection: &Connection) -> rusqlite::Result<(i64, i64)> {
    let indexed: i64 =
        connection.query_row("SELECT count(*) FROM message_search", [], |row| row.get(0))?;
    let total: i64 = connection.query_row("SELECT count(*) FROM messages", [], |row| row.get(0))?;
    Ok((indexed, total))
}

/// Copies up to `limit` not-yet-indexed messages. Returns how many were copied.
pub(super) fn index_batch(connection: &Connection, limit: i64) -> rusqlite::Result<usize> {
    let expr = body_expr("m.content");
    let transaction = connection.unchecked_transaction()?;
    let copied = transaction.execute(
        &format!(
            "INSERT INTO message_search(rowid, body)
             SELECT m.rowid, {expr}
             FROM messages AS m
             WHERE NOT EXISTS (SELECT 1 FROM message_search AS s WHERE s.rowid = m.rowid)
             LIMIT ?1"
        ),
        params![limit],
    )?;
    transaction.commit()?;
    Ok(copied)
}

/// A quoted trigram phrase. Quotes inside the needle are escaped.
pub(super) fn phrase(needle: &str) -> String {
    format!("\"{}\"", needle.replace('"', "\"\""))
}

pub(super) fn query(
    connection: &Connection,
    chat: Option<&str>,
    needle: &str,
    limit: usize,
) -> rusqlite::Result<Vec<Message>> {
    let mut statement = connection.prepare_cached(
        "SELECT messages.chat, messages.id, messages.sender, messages.sender_name, messages.from_me,
                messages.timestamp, messages.content, messages.status, messages.quoted, messages.reactions,
                messages.edited, messages.thumbnail, messages.mentions, messages.forwarded,
                messages.delivered_at, messages.read_at
         FROM message_fts
         JOIN messages ON messages.rowid = message_fts.rowid
         WHERE message_fts MATCH ?1
           AND json_valid(messages.content)
           AND (?3 IS NULL OR messages.chat = ?3)
         ORDER BY messages.timestamp DESC, messages.rowid DESC
         LIMIT ?2",
    )?;
    let rows = statement.query_map(params![phrase(needle), limit as i64, chat], map_row)?;
    rows.collect()
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
    let chat: String = row.get(0)?;
    let content: String = row.get(6)?;
    let quoted: Option<String> = row.get(8)?;
    let reactions: String = row.get(9)?;
    let mentions: String = row.get(12)?;
    Ok(Message {
        id: row.get(1)?,
        chat,
        sender: row.get(2)?,
        sender_name: row.get(3)?,
        from_me: row.get(4)?,
        timestamp: row.get(5)?,
        content: serde_json::from_str(&content).unwrap_or(Content::Unsupported {
            what: "unreadable".into(),
            reason: "unknown".into(),
        }),
        status: status_from_rank(row.get(7)?),
        delivered_at: row.get(14)?,
        read_at: row.get(15)?,
        quoted: quoted.and_then(|quoted| serde_json::from_str(&quoted).ok()),
        reactions: serde_json::from_str(&reactions).unwrap_or_default(),
        edited: row.get(10)?,
        mentions: serde_json::from_str(&mentions).unwrap_or_default(),
        forwarded: row.get(13)?,
        thumbnail: row.get(11)?,
    })
}
