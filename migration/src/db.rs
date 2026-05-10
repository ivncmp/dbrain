use crate::config::Config;
use anyhow::{Context, Result};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;
use std::path::PathBuf;

pub type Db = Pool<SqliteConnectionManager>;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS entities (
  id          TEXT PRIMARY KEY,
  name        TEXT NOT NULL,
  type        TEXT NOT NULL,
  category    TEXT NOT NULL,
  status      TEXT DEFAULT 'active',
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL,
  metadata    TEXT
);

CREATE TABLE IF NOT EXISTS facts (
  id               TEXT PRIMARY KEY,
  entity_id        TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
  fact             TEXT NOT NULL,
  category         TEXT NOT NULL,
  timestamp        TEXT NOT NULL,
  status           TEXT DEFAULT 'active',
  superseded_by    TEXT,
  related_entities TEXT DEFAULT '[]',
  last_accessed    TEXT NOT NULL,
  access_count     INTEGER DEFAULT 0,
  tier             TEXT DEFAULT 'warm',
  source           TEXT
);

CREATE VIRTUAL TABLE IF NOT EXISTS facts_fts USING fts5(
  fact, entity_id, category,
  content='facts', content_rowid='rowid'
);

CREATE TABLE IF NOT EXISTS documents (
  key         TEXT PRIMARY KEY,
  title       TEXT NOT NULL,
  content     TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS conversations (
  id          TEXT PRIMARY KEY,
  source      TEXT NOT NULL,
  started_at  TEXT NOT NULL,
  ended_at    TEXT,
  summary     TEXT
);

CREATE TABLE IF NOT EXISTS messages (
  id              TEXT PRIMARY KEY,
  conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
  role            TEXT NOT NULL,
  content         TEXT NOT NULL,
  timestamp       TEXT NOT NULL,
  processed       INTEGER DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_messages_conversation ON messages(conversation_id, timestamp);
CREATE INDEX IF NOT EXISTS idx_messages_unprocessed ON messages(processed) WHERE processed = 0;

CREATE INDEX IF NOT EXISTS idx_facts_entity ON facts(entity_id);
CREATE INDEX IF NOT EXISTS idx_facts_tier ON facts(tier, last_accessed DESC);

CREATE TRIGGER IF NOT EXISTS facts_ai AFTER INSERT ON facts BEGIN
  INSERT INTO facts_fts(rowid, fact, entity_id, category)
    VALUES (new.rowid, new.fact, new.entity_id, new.category);
END;

CREATE TRIGGER IF NOT EXISTS facts_ad AFTER DELETE ON facts BEGIN
  INSERT INTO facts_fts(facts_fts, rowid, fact, entity_id, category)
    VALUES('delete', old.rowid, old.fact, old.entity_id, old.category);
END;

CREATE TRIGGER IF NOT EXISTS facts_au AFTER UPDATE ON facts BEGIN
  INSERT INTO facts_fts(facts_fts, rowid, fact, entity_id, category)
    VALUES('delete', old.rowid, old.fact, old.entity_id, old.category);
  INSERT INTO facts_fts(rowid, fact, entity_id, category)
    VALUES (new.rowid, new.fact, new.entity_id, new.category);
END;
"#;

/// Apply pragmas to a freshly opened connection.
/// Called both during pool creation (init callback) and for the schema connection.
fn apply_pragmas(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")
        .context("Failed to enable WAL mode")?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .context("Failed to set synchronous mode")?;
    conn.pragma_update(None, "busy_timeout", "5000")
        .context("Failed to set busy timeout")?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .context("Failed to enable foreign keys")?;
    Ok(())
}

pub fn create_database(config: &Config) -> Result<Db> {
    let mut db_path = PathBuf::from(&config.data_path);
    db_path.push("dbrain.db");

    // Apply schema with a one-off connection first
    {
        let conn = Connection::open(&db_path)
            .with_context(|| format!("Failed to open database at {}", db_path.display()))?;
        apply_pragmas(&conn)?;
        conn.execute_batch(SCHEMA)
            .context("Failed to apply schema")?;
    }

    // Build connection pool
    let manager = SqliteConnectionManager::file(&db_path);
    let pool = Pool::builder()
        .max_size(8)
        .connection_customizer(Box::new(PragmaCustomizer))
        .build(manager)
        .context("Failed to build connection pool")?;

    Ok(pool)
}

/// Customizer that applies pragmas to every new pool connection.
#[derive(Debug)]
struct PragmaCustomizer;

impl r2d2::CustomizeConnection<Connection, rusqlite::Error> for PragmaCustomizer {
    fn on_acquire(&self, conn: &mut Connection) -> Result<(), rusqlite::Error> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "busy_timeout", "5000")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(())
    }
}
