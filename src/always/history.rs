//! Dictation history persistence — raw + polished transcripts, searchable.
//!
//! Stores all dictation events in SQLite for later retrieval via the Swift UI.
//! Uses keyset pagination (not OFFSET) for efficient unbounded table growth.

use anyhow::Result;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

use super::postprocess::TransformStyle;

/// A single dictation history entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: i64,
    pub created_at_ms: i64,
    pub raw_text: String,
    pub polished_text: String,
    pub word_count: i64,
    pub duration_ms: Option<i64>,
    pub app_bundle_id: Option<String>,
}

/// Session statistics derived from history (for Feature 4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStats {
    pub total_words: i64,
    pub total_utterances: i64,
    pub total_duration_ms: i64,
    pub wpm: f64, // words per minute
}

/// History store wrapping the shared SQLite connection.
#[derive(Debug)]
pub struct HistoryStore {
    conn: Arc<Mutex<Connection>>,
}

impl HistoryStore {
    /// Create a new history store from a shared connection.
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    /// Insert a new dictation history entry.
    pub fn insert(
        &self,
        raw_text: String,
        polished_text: String,
        duration_ms: Option<i64>,
        app_bundle_id: Option<String>,
    ) -> Result<i64> {
        let word_count = polished_text.split_whitespace().count() as i64;
        let created_at_ms = chrono::Utc::now().timestamp_millis();

        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Failed to lock connection: {}", e))?;
        conn.execute(
            "INSERT INTO dictation_history (created_at_ms, raw_text, polished_text, word_count, duration_ms, app_bundle_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![created_at_ms, raw_text, polished_text, word_count, duration_ms, app_bundle_id],
        )?;
        let id = conn.last_insert_rowid();
        Ok(id)
    }

    /// Fetch a page of history entries using keyset pagination.
    /// `before_id` is the exclusive upper bound (None = first page).
    pub fn page(&self, limit: i64, before_id: Option<i64>) -> Result<Vec<HistoryEntry>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Failed to lock connection: {}", e))?;

        let entries = if let Some(before) = before_id {
            let mut stmt = conn.prepare(
                "SELECT id, created_at_ms, raw_text, polished_text, word_count, duration_ms, app_bundle_id 
                 FROM dictation_history 
                 WHERE id < ?1 
                 ORDER BY id DESC 
                 LIMIT ?2",
            )?;
            stmt.query_map([before, limit], |row| {
                Ok(HistoryEntry {
                    id: row.get(0)?,
                    created_at_ms: row.get(1)?,
                    raw_text: row.get(2)?,
                    polished_text: row.get(3)?,
                    word_count: row.get(4)?,
                    duration_ms: row.get(5)?,
                    app_bundle_id: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
        } else {
            let mut stmt = conn.prepare(
                "SELECT id, created_at_ms, raw_text, polished_text, word_count, duration_ms, app_bundle_id 
                 FROM dictation_history 
                 ORDER BY id DESC 
                 LIMIT ?1",
            )?;
            stmt.query_map([limit], |row| {
                Ok(HistoryEntry {
                    id: row.get(0)?,
                    created_at_ms: row.get(1)?,
                    raw_text: row.get(2)?,
                    polished_text: row.get(3)?,
                    word_count: row.get(4)?,
                    duration_ms: row.get(5)?,
                    app_bundle_id: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
        };

        Ok(entries)
    }

    /// Search history entries by query (LIKE on both text columns).
    pub fn search(&self, query: &str, limit: i64) -> Result<Vec<HistoryEntry>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Failed to lock connection: {}", e))?;

        let pattern = format!("%{}%", query);
        let mut stmt = conn.prepare(
            "SELECT id, created_at_ms, raw_text, polished_text, word_count, duration_ms, app_bundle_id 
             FROM dictation_history 
             WHERE raw_text LIKE ?1 OR polished_text LIKE ?1 
             ORDER BY id DESC 
             LIMIT ?2",
        )?;
        let entries = stmt
            .query_map([&pattern, &limit.to_string()], |row| {
                Ok(HistoryEntry {
                    id: row.get(0)?,
                    created_at_ms: row.get(1)?,
                    raw_text: row.get(2)?,
                    polished_text: row.get(3)?,
                    word_count: row.get(4)?,
                    duration_ms: row.get(5)?,
                    app_bundle_id: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(entries)
    }

    /// Compute session statistics for a time range.
    pub fn stats_for_range(&self, since_ms: i64) -> Result<SessionStats> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Failed to lock connection: {}", e))?;

        let mut stmt = conn.prepare(
            "SELECT SUM(word_count), COUNT(*), SUM(duration_ms) 
             FROM dictation_history 
             WHERE created_at_ms >= ?1",
        )?;
        let (total_words, total_utterances, total_duration_ms) =
            stmt.query_row([&since_ms.to_string()], |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?.unwrap_or(0),
                    row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                ))
            })?;

        let wpm = if total_duration_ms > 0 {
            (total_words as f64) / ((total_duration_ms as f64) / 60_000.0)
        } else {
            0.0
        };

        Ok(SessionStats {
            total_words,
            total_utterances,
            total_duration_ms,
            wpm,
        })
    }

    /// Fetch a single history entry by ID.
    pub fn get_by_id(&self, id: i64) -> Result<HistoryEntry> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Failed to lock connection: {}", e))?;

        let mut stmt = conn.prepare(
            "SELECT id, created_at_ms, raw_text, polished_text, word_count, duration_ms, app_bundle_id 
             FROM dictation_history 
             WHERE id = ?1",
        )?;
        let entry = stmt.query_row([id], |row| {
            Ok(HistoryEntry {
                id: row.get(0)?,
                created_at_ms: row.get(1)?,
                raw_text: row.get(2)?,
                polished_text: row.get(3)?,
                word_count: row.get(4)?,
                duration_ms: row.get(5)?,
                app_bundle_id: row.get(6)?,
            })
        })?;
        Ok(entry)
    }

    /// Upsert a style variant for a history entry.
    pub fn upsert_style_variant(
        &self,
        history_id: i64,
        style: TransformStyle,
        text: String,
    ) -> Result<()> {
        let created_at_ms = chrono::Utc::now().timestamp_millis();
        let style_str = style.to_string();

        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Failed to lock connection: {}", e))?;
        conn.execute(
            "INSERT INTO history_style_variants (history_id, style, text, created_at_ms) 
             VALUES (?1, ?2, ?3, ?4) 
             ON CONFLICT(history_id, style) DO UPDATE SET text = ?3, created_at_ms = ?4",
            params![history_id, style_str, text, created_at_ms],
        )?;
        Ok(())
    }

    /// Get a style variant for a history entry.
    pub fn get_style_variant(
        &self,
        history_id: i64,
        style: TransformStyle,
    ) -> Result<Option<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Failed to lock connection: {}", e))?;
        let style_str = style.to_string();

        let result = conn.query_row(
            "SELECT text FROM history_style_variants WHERE history_id = ?1 AND style = ?2",
            params![history_id, style_str],
            |row| row.get::<_, String>(0),
        );
        match result {
            Ok(text) => Ok(Some(text)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};

    fn setup_test_db() -> Arc<Mutex<Connection>> {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE dictation_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at_ms INTEGER NOT NULL,
                raw_text TEXT NOT NULL,
                polished_text TEXT NOT NULL,
                word_count INTEGER NOT NULL,
                duration_ms INTEGER,
                app_bundle_id TEXT
            );
            CREATE INDEX idx_history_created_at ON dictation_history(created_at_ms DESC);
            CREATE TABLE history_style_variants (
                history_id INTEGER NOT NULL REFERENCES dictation_history(id),
                style TEXT NOT NULL,
                text TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                PRIMARY KEY (history_id, style)
            );",
        )
        .unwrap();
        Arc::new(Mutex::new(conn))
    }

    #[test]
    fn test_insert_and_page() {
        let store = HistoryStore::new(setup_test_db());

        let id1 = store
            .insert(
                "hello world".to_string(),
                "Hello, world!".to_string(),
                Some(1000),
                Some("com.apple.TextEdit".to_string()),
            )
            .unwrap();

        let id2 = store
            .insert(
                "test dictation".to_string(),
                "Test dictation.".to_string(),
                Some(2000),
                Some("com.apple.Safari".to_string()),
            )
            .unwrap();

        let entries = store.page(10, None).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, id2); // Most recent first
        assert_eq!(entries[1].id, id1);
    }

    #[test]
    fn test_keyset_pagination() {
        let store = HistoryStore::new(setup_test_db());

        for i in 0..5 {
            store
                .insert(
                    format!("entry {}", i),
                    format!("Entry {}.", i),
                    Some(1000),
                    None,
                )
                .unwrap();
        }

        let page1 = store.page(2, None).unwrap();
        assert_eq!(page1.len(), 2);

        let page2 = store.page(2, Some(page1[1].id)).unwrap();
        assert_eq!(page2.len(), 2);

        let page3 = store.page(2, Some(page2[1].id)).unwrap();
        assert_eq!(page3.len(), 1); // Only one left
    }

    #[test]
    fn test_search() {
        let store = HistoryStore::new(setup_test_db());

        store
            .insert(
                "hello world".to_string(),
                "Hello, world!".to_string(),
                Some(1000),
                None,
            )
            .unwrap();

        store
            .insert(
                "test dictation".to_string(),
                "Test dictation.".to_string(),
                Some(2000),
                None,
            )
            .unwrap();

        let results = store.search("hello", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].raw_text.contains("hello"));
    }

    #[test]
    fn test_stats() {
        let store = HistoryStore::new(setup_test_db());

        store
            .insert(
                "one two three".to_string(),
                "One two three.".to_string(),
                Some(60000), // 1 minute
                None,
            )
            .unwrap();

        store
            .insert(
                "four five six".to_string(),
                "Four five six.".to_string(),
                Some(60000), // 1 minute
                None,
            )
            .unwrap();

        let stats = store.stats_for_range(0).unwrap();
        assert_eq!(stats.total_words, 6);
        assert_eq!(stats.total_utterances, 2);
        assert_eq!(stats.total_duration_ms, 120000);
        assert!((stats.wpm - 3.0).abs() < 0.1); // 6 words / 2 minutes
    }

    #[test]
    fn test_insert_with_none_values() {
        let store = HistoryStore::new(setup_test_db());

        // Insert with None for both optional fields (matches production usage)
        let id = store
            .insert(
                "hello world".to_string(),
                "Hello, world!".to_string(),
                None, // duration_ms
                None, // app_bundle_id
            )
            .unwrap();

        // Verify we can read it back without errors
        let entries = store.page(10, None).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, id);
        assert_eq!(entries[0].duration_ms, None);
        assert_eq!(entries[0].app_bundle_id, None);

        // Verify search also works with None values
        let results = store.search("hello", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].duration_ms, None);
    }

    #[test]
    fn test_get_by_id() {
        let store = HistoryStore::new(setup_test_db());

        let id = store
            .insert(
                "hello world".to_string(),
                "Hello, world!".to_string(),
                Some(1000),
                Some("com.apple.TextEdit".to_string()),
            )
            .unwrap();

        let entry = store.get_by_id(id).unwrap();
        assert_eq!(entry.id, id);
        assert_eq!(entry.raw_text, "hello world");
        assert_eq!(entry.polished_text, "Hello, world!");
    }

    #[test]
    fn test_upsert_and_get_style_variant() {
        let store = HistoryStore::new(setup_test_db());

        let history_id = store
            .insert(
                "hello world".to_string(),
                "Hello, world!".to_string(),
                Some(1000),
                None,
            )
            .unwrap();

        // Insert a style variant
        store
            .upsert_style_variant(
                history_id,
                TransformStyle::Formal,
                "Hello, world.".to_string(),
            )
            .unwrap();

        // Retrieve it
        let variant = store
            .get_style_variant(history_id, TransformStyle::Formal)
            .unwrap();
        assert_eq!(variant, Some("Hello, world.".to_string()));

        // Update the same variant (upsert)
        store
            .upsert_style_variant(
                history_id,
                TransformStyle::Formal,
                "Hello, world!".to_string(),
            )
            .unwrap();
        let updated = store
            .get_style_variant(history_id, TransformStyle::Formal)
            .unwrap();
        assert_eq!(updated, Some("Hello, world!".to_string()));

        // Non-existent variant returns None
        let missing = store
            .get_style_variant(history_id, TransformStyle::KeyPoints)
            .unwrap();
        assert_eq!(missing, None);
    }
}
