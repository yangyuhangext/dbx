use std::path::Path;
use std::str::FromStr;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};

use crate::ai::{AiChatMessage, AiConfig, AiConversation};
use crate::handoff::HandoffItem;
use crate::history::HistoryEntry;
use crate::models::connection::ConnectionConfig;
use crate::saved_sql::{SavedSqlFile, SavedSqlFolder, SavedSqlLibrary};

pub struct Storage {
    db: SqlitePool,
}

const SCHEMA_STATEMENTS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS connections (
        id TEXT PRIMARY KEY,
        config_json TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS connection_secrets (
        connection_id TEXT NOT NULL,
        key TEXT NOT NULL,
        secret TEXT NOT NULL,
        PRIMARY KEY (connection_id, key)
    )",
    "CREATE TABLE IF NOT EXISTS history (
        id TEXT PRIMARY KEY,
        connection_id TEXT NOT NULL DEFAULT '',
        connection_name TEXT NOT NULL DEFAULT '',
        database TEXT NOT NULL DEFAULT '',
        sql_text TEXT NOT NULL DEFAULT '',
        executed_at TEXT NOT NULL DEFAULT '',
        execution_time_ms INTEGER NOT NULL DEFAULT 0,
        success INTEGER NOT NULL DEFAULT 1,
        error TEXT,
        activity_kind TEXT NOT NULL DEFAULT 'query',
        operation TEXT NOT NULL DEFAULT '',
        target TEXT NOT NULL DEFAULT '',
        affected_rows INTEGER,
        rollback_sql TEXT,
        details_json TEXT
    )",
    "CREATE TABLE IF NOT EXISTS ai_config (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        config_json TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS ai_conversations (
        id TEXT PRIMARY KEY,
        title TEXT NOT NULL DEFAULT '',
        connection_name TEXT NOT NULL DEFAULT '',
        database TEXT NOT NULL DEFAULT '',
        messages_json TEXT NOT NULL DEFAULT '[]',
        created_at TEXT NOT NULL DEFAULT '',
        updated_at TEXT NOT NULL DEFAULT ''
    )",
    "CREATE TABLE IF NOT EXISTS sidebar_layout (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        layout_json TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS app_settings (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        settings_json TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS schema_cache (
        cache_key TEXT PRIMARY KEY,
        payload_json TEXT NOT NULL,
        updated_at TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS saved_sql_folders (
        id TEXT PRIMARY KEY,
        connection_id TEXT NOT NULL,
        name TEXT NOT NULL DEFAULT '',
        created_at TEXT NOT NULL DEFAULT '',
        updated_at TEXT NOT NULL DEFAULT ''
    )",
    "CREATE TABLE IF NOT EXISTS saved_sql_files (
        id TEXT PRIMARY KEY,
        connection_id TEXT NOT NULL,
        folder_id TEXT,
        name TEXT NOT NULL DEFAULT '',
        database_name TEXT NOT NULL DEFAULT '',
        schema_name TEXT,
        sql_text TEXT NOT NULL DEFAULT '',
        created_at TEXT NOT NULL DEFAULT '',
        updated_at TEXT NOT NULL DEFAULT ''
    )",
    "CREATE TABLE IF NOT EXISTS handoffs (
        seq INTEGER PRIMARY KEY AUTOINCREMENT,
        id TEXT NOT NULL UNIQUE,
        payload_json TEXT NOT NULL,
        status TEXT NOT NULL,
        created_at TEXT NOT NULL
    )",
];

// ---------------------------------------------------------------------------
// Construction / schema
// ---------------------------------------------------------------------------

impl Storage {
    pub async fn open(db_path: &Path) -> Result<Self, String> {
        let url = format!("sqlite:{}?mode=rwc", db_path.display());
        let options = SqliteConnectOptions::from_str(&url).map_err(|e| e.to_string())?.create_if_missing(true);
        let pool =
            SqlitePoolOptions::new().max_connections(5).connect_with(options).await.map_err(|e| e.to_string())?;

        for statement in SCHEMA_STATEMENTS {
            sqlx::query(statement).execute(&pool).await.map_err(|e| e.to_string())?;
        }
        ensure_history_columns(&pool).await?;
        ensure_handoffs_sequence(&pool).await?;

        Ok(Self { db: pool })
    }
}

async fn ensure_history_columns(pool: &SqlitePool) -> Result<(), String> {
    const COLUMNS: &[(&str, &str)] = &[
        ("activity_kind", "TEXT NOT NULL DEFAULT 'query'"),
        ("connection_id", "TEXT NOT NULL DEFAULT ''"),
        ("operation", "TEXT NOT NULL DEFAULT ''"),
        ("target", "TEXT NOT NULL DEFAULT ''"),
        ("affected_rows", "INTEGER"),
        ("rollback_sql", "TEXT"),
        ("details_json", "TEXT"),
    ];

    let rows: Vec<(String,)> = sqlx::query_as("SELECT name FROM pragma_table_info('history')")
        .fetch_all(pool)
        .await
        .map_err(|e| e.to_string())?;
    let existing: std::collections::HashSet<String> = rows.into_iter().map(|(name,)| name).collect();
    for (name, definition) in COLUMNS {
        if existing.contains(*name) {
            continue;
        }
        sqlx::query(&format!("ALTER TABLE history ADD COLUMN {name} {definition}"))
            .execute(pool)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

async fn ensure_handoffs_sequence(pool: &SqlitePool) -> Result<(), String> {
    let (seq_columns,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM pragma_table_info('handoffs') WHERE name = 'seq'")
            .fetch_one(pool)
            .await
            .map_err(|e| e.to_string())?;

    if seq_columns > 0 {
        return Ok(());
    }

    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    sqlx::query(
        "CREATE TABLE handoffs_migration (
            seq INTEGER PRIMARY KEY AUTOINCREMENT,
            id TEXT NOT NULL UNIQUE,
            payload_json TEXT NOT NULL,
            status TEXT NOT NULL,
            created_at TEXT NOT NULL
        )",
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;
    sqlx::query(
        "INSERT INTO handoffs_migration (id, payload_json, status, created_at)
         SELECT id, payload_json, status, created_at
         FROM handoffs
         ORDER BY created_at ASC, rowid ASC",
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;
    sqlx::query("DROP TABLE handoffs").execute(&mut *tx).await.map_err(|e| e.to_string())?;
    sqlx::query("ALTER TABLE handoffs_migration RENAME TO handoffs")
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct HistoryRow {
    id: String,
    connection_id: String,
    connection_name: String,
    database: String,
    sql_text: String,
    executed_at: String,
    execution_time_ms: i64,
    success: bool,
    error: Option<String>,
    activity_kind: String,
    operation: String,
    target: String,
    affected_rows: Option<i64>,
    rollback_sql: Option<String>,
    details_json: Option<String>,
}

impl Storage {
    pub async fn save_history_entry(&self, entry: &HistoryEntry) -> Result<(), String> {
        sqlx::query(
            "INSERT OR REPLACE INTO history \
             (id, connection_name, database, sql_text, executed_at, execution_time_ms, success, error, \
              activity_kind, connection_id, operation, target, affected_rows, rollback_sql, details_json) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&entry.id)
        .bind(&entry.connection_name)
        .bind(&entry.database)
        .bind(&entry.sql)
        .bind(&entry.executed_at)
        .bind(entry.execution_time_ms as i64)
        .bind(entry.success)
        .bind(&entry.error)
        .bind(&entry.activity_kind)
        .bind(&entry.connection_id)
        .bind(&entry.operation)
        .bind(&entry.target)
        .bind(entry.affected_rows)
        .bind(&entry.rollback_sql)
        .bind(&entry.details_json)
        .execute(&self.db)
        .await
        .map_err(|e| e.to_string())?;

        // Keep at most MAX_HISTORY entries
        sqlx::query(
            "DELETE FROM history WHERE id NOT IN \
             (SELECT id FROM history ORDER BY executed_at DESC LIMIT 1000)",
        )
        .execute(&self.db)
        .await
        .map_err(|e| e.to_string())?;

        Ok(())
    }

    pub async fn load_history_entries(&self, limit: usize, offset: usize) -> Result<Vec<HistoryEntry>, String> {
        let rows: Vec<HistoryRow> = sqlx::query_as(
            "SELECT id, connection_name, database, sql_text, executed_at, \
             execution_time_ms, success, error, activity_kind, connection_id, operation, target, \
             affected_rows, rollback_sql, details_json \
             FROM history ORDER BY executed_at DESC LIMIT ? OFFSET ?",
        )
        .bind(limit as i64)
        .bind(offset as i64)
        .fetch_all(&self.db)
        .await
        .map_err(|e| e.to_string())?;

        Ok(rows
            .into_iter()
            .map(|r| HistoryEntry {
                id: r.id,
                connection_id: r.connection_id,
                connection_name: r.connection_name,
                database: r.database,
                sql: r.sql_text,
                executed_at: r.executed_at,
                execution_time_ms: r.execution_time_ms as u128,
                success: r.success,
                error: r.error,
                activity_kind: if r.activity_kind.is_empty() { "query".to_string() } else { r.activity_kind },
                operation: r.operation,
                target: r.target,
                affected_rows: r.affected_rows,
                rollback_sql: r.rollback_sql,
                details_json: r.details_json,
            })
            .collect())
    }

    pub async fn clear_history(&self) -> Result<(), String> {
        sqlx::query("DELETE FROM history").execute(&self.db).await.map_err(|e| e.to_string())?;
        Ok(())
    }

    pub async fn delete_history_entry(&self, id: &str) -> Result<(), String> {
        sqlx::query("DELETE FROM history WHERE id = ?").bind(id).execute(&self.db).await.map_err(|e| e.to_string())?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Handoffs
// ---------------------------------------------------------------------------

impl Storage {
    pub async fn save_handoff(&self, item: &HandoffItem) -> Result<(), String> {
        let json = serde_json::to_string(item).map_err(|e| e.to_string())?;
        let status = serde_json::to_value(&item.status)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .ok_or_else(|| "Failed to serialize handoff status".to_string())?;

        sqlx::query(
            "INSERT INTO handoffs (id, payload_json, status, created_at) \
             VALUES (?, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET \
             payload_json = excluded.payload_json, \
             status = excluded.status, \
             created_at = excluded.created_at",
        )
        .bind(&item.id)
        .bind(json)
        .bind(status)
        .bind(item.created_at.to_rfc3339())
        .execute(&self.db)
        .await
        .map_err(|e| e.to_string())?;

        Ok(())
    }

    pub async fn load_pending_handoffs(&self) -> Result<Vec<HandoffItem>, String> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT payload_json FROM handoffs \
             WHERE status IN ('queued', 'shown') \
             ORDER BY created_at ASC, seq ASC",
        )
        .fetch_all(&self.db)
        .await
        .map_err(|e| e.to_string())?;

        rows.into_iter().map(|(json,)| serde_json::from_str(&json).map_err(|e| e.to_string())).collect()
    }
}

// ---------------------------------------------------------------------------
// AI Config
// ---------------------------------------------------------------------------

impl Storage {
    pub async fn save_ai_config(&self, config: &AiConfig) -> Result<(), String> {
        let json = serde_json::to_string(config).map_err(|e| e.to_string())?;
        sqlx::query("INSERT OR REPLACE INTO ai_config (id, config_json) VALUES (1, ?)")
            .bind(&json)
            .execute(&self.db)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub async fn load_ai_config(&self) -> Result<Option<AiConfig>, String> {
        let row: Option<(String,)> = sqlx::query_as("SELECT config_json FROM ai_config WHERE id = 1")
            .fetch_optional(&self.db)
            .await
            .map_err(|e| e.to_string())?;
        match row {
            Some((json,)) => serde_json::from_str(&json).map(Some).map_err(|e| e.to_string()),
            None => Ok(None),
        }
    }
}

// ---------------------------------------------------------------------------
// App Settings
// ---------------------------------------------------------------------------

impl Storage {
    pub async fn save_password_hash(&self, hash: &str) -> Result<(), String> {
        let json = serde_json::json!({ "password_hash": hash }).to_string();
        sqlx::query("INSERT OR REPLACE INTO app_settings (id, settings_json) VALUES (1, ?)")
            .bind(&json)
            .execute(&self.db)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub async fn load_password_hash(&self) -> Result<Option<String>, String> {
        let row: Option<(String,)> = sqlx::query_as("SELECT settings_json FROM app_settings WHERE id = 1")
            .fetch_optional(&self.db)
            .await
            .map_err(|e| e.to_string())?;
        match row {
            Some((json,)) => {
                let v: serde_json::Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;
                Ok(v.get("password_hash").and_then(|v| v.as_str()).map(|s| s.to_string()))
            }
            None => Ok(None),
        }
    }
}

// ---------------------------------------------------------------------------
// AI Conversations
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct AiConversationRow {
    id: String,
    title: String,
    connection_name: String,
    database: String,
    messages_json: String,
    created_at: String,
    updated_at: String,
}

impl Storage {
    pub async fn save_ai_conversation(&self, conv: &AiConversation) -> Result<(), String> {
        let messages_json = serde_json::to_string(&conv.messages).map_err(|e| e.to_string())?;
        sqlx::query(
            "INSERT OR REPLACE INTO ai_conversations \
             (id, title, connection_name, database, messages_json, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&conv.id)
        .bind(&conv.title)
        .bind(&conv.connection_name)
        .bind(&conv.database)
        .bind(&messages_json)
        .bind(&conv.created_at)
        .bind(&conv.updated_at)
        .execute(&self.db)
        .await
        .map_err(|e| e.to_string())?;

        // Keep at most 50 conversations
        sqlx::query(
            "DELETE FROM ai_conversations WHERE id NOT IN \
             (SELECT id FROM ai_conversations ORDER BY updated_at DESC LIMIT 50)",
        )
        .execute(&self.db)
        .await
        .map_err(|e| e.to_string())?;

        Ok(())
    }

    pub async fn load_ai_conversations(&self) -> Result<Vec<AiConversation>, String> {
        let rows: Vec<AiConversationRow> = sqlx::query_as(
            "SELECT id, title, connection_name, database, messages_json, \
             created_at, updated_at \
             FROM ai_conversations ORDER BY updated_at DESC",
        )
        .fetch_all(&self.db)
        .await
        .map_err(|e| e.to_string())?;

        rows.into_iter()
            .map(|r| {
                let messages: Vec<AiChatMessage> = serde_json::from_str(&r.messages_json).map_err(|e| e.to_string())?;
                Ok(AiConversation {
                    id: r.id,
                    title: r.title,
                    connection_name: r.connection_name,
                    database: r.database,
                    messages,
                    created_at: r.created_at,
                    updated_at: r.updated_at,
                })
            })
            .collect()
    }

    pub async fn delete_ai_conversation(&self, id: &str) -> Result<(), String> {
        sqlx::query("DELETE FROM ai_conversations WHERE id = ?")
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Connections (with inline secrets)
// ---------------------------------------------------------------------------

impl Storage {
    pub async fn save_connections(&self, configs: &[ConnectionConfig]) -> Result<(), String> {
        let mut tx = self.db.begin().await.map_err(|e| e.to_string())?;

        sqlx::query("DELETE FROM connections").execute(&mut *tx).await.map_err(|e| e.to_string())?;

        for config in configs {
            // Store config without secrets
            let mut sanitized = config.clone();
            sanitized.password = String::new();
            sanitized.ssh_password = String::new();
            sanitized.ssh_key_passphrase = String::new();
            sanitized.proxy_password = String::new();
            sanitized.connection_string = None;
            let json = serde_json::to_string(&sanitized).map_err(|e| e.to_string())?;

            sqlx::query("INSERT INTO connections (id, config_json) VALUES (?, ?)")
                .bind(&config.id)
                .bind(&json)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;

            // Store secrets
            persist_secret_in_tx(&mut tx, &config.id, "password", &config.password).await?;
            persist_secret_in_tx(&mut tx, &config.id, "ssh_password", &config.ssh_password).await?;
            persist_secret_in_tx(&mut tx, &config.id, "ssh_key_passphrase", &config.ssh_key_passphrase).await?;
            persist_secret_in_tx(&mut tx, &config.id, "proxy_password", &config.proxy_password).await?;
            if let Some(cs) = &config.connection_string {
                persist_secret_in_tx(&mut tx, &config.id, "connection_string", cs).await?;
            } else {
                sqlx::query("DELETE FROM connection_secrets WHERE connection_id = ? AND key = ?")
                    .bind(&config.id)
                    .bind("connection_string")
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| e.to_string())?;
            }
        }

        // Remove secrets for connections that no longer exist
        if configs.is_empty() {
            sqlx::query("DELETE FROM connection_secrets").execute(&mut *tx).await.map_err(|e| e.to_string())?;
        } else {
            let placeholders: Vec<&str> = configs.iter().map(|_| "?").collect();
            let sql = format!("DELETE FROM connection_secrets WHERE connection_id NOT IN ({})", placeholders.join(","));
            let mut query = sqlx::query(&sql);
            for config in configs {
                query = query.bind(&config.id);
            }
            query.execute(&mut *tx).await.map_err(|e| e.to_string())?;
        }

        tx.commit().await.map_err(|e| e.to_string())?;
        Ok(())
    }

    pub async fn load_connections(&self) -> Result<Vec<ConnectionConfig>, String> {
        let rows: Vec<(String, String)> = sqlx::query_as("SELECT id, config_json FROM connections")
            .fetch_all(&self.db)
            .await
            .map_err(|e| e.to_string())?;

        let mut configs = Vec::new();
        for (id, json) in rows {
            let mut config: ConnectionConfig = serde_json::from_str(&json).map_err(|e| e.to_string())?;
            config.password = self.get_secret(&id, "password").await?.unwrap_or_default();
            config.ssh_password = self.get_secret(&id, "ssh_password").await?.unwrap_or_default();
            config.ssh_key_passphrase = self.get_secret(&id, "ssh_key_passphrase").await?.unwrap_or_default();
            config.proxy_password = self.get_secret(&id, "proxy_password").await?.unwrap_or_default();
            config.connection_string = self.get_secret(&id, "connection_string").await?;
            configs.push(config);
        }
        Ok(configs)
    }
}

// ---------------------------------------------------------------------------
// Saved SQL Library
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct SavedSqlFolderRow {
    id: String,
    connection_id: String,
    name: String,
    created_at: String,
    updated_at: String,
}

#[derive(sqlx::FromRow)]
struct SavedSqlFileRow {
    id: String,
    connection_id: String,
    folder_id: Option<String>,
    name: String,
    database_name: String,
    schema_name: Option<String>,
    sql_text: String,
    created_at: String,
    updated_at: String,
}

impl From<SavedSqlFolderRow> for SavedSqlFolder {
    fn from(row: SavedSqlFolderRow) -> Self {
        Self {
            id: row.id,
            connection_id: row.connection_id,
            name: row.name,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

impl From<SavedSqlFileRow> for SavedSqlFile {
    fn from(row: SavedSqlFileRow) -> Self {
        Self {
            id: row.id,
            connection_id: row.connection_id,
            folder_id: row.folder_id,
            name: row.name,
            database: row.database_name,
            schema: row.schema_name,
            sql: row.sql_text,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

impl Storage {
    pub async fn load_saved_sql_library(&self) -> Result<SavedSqlLibrary, String> {
        let folder_rows: Vec<SavedSqlFolderRow> = sqlx::query_as(
            "SELECT id, connection_id, name, created_at, updated_at \
             FROM saved_sql_folders ORDER BY connection_id, name COLLATE NOCASE",
        )
        .fetch_all(&self.db)
        .await
        .map_err(|e| e.to_string())?;

        let file_rows: Vec<SavedSqlFileRow> = sqlx::query_as(
            "SELECT id, connection_id, folder_id, name, database_name, schema_name, sql_text, created_at, updated_at \
             FROM saved_sql_files ORDER BY connection_id, folder_id, name COLLATE NOCASE",
        )
        .fetch_all(&self.db)
        .await
        .map_err(|e| e.to_string())?;

        Ok(SavedSqlLibrary {
            folders: folder_rows.into_iter().map(Into::into).collect(),
            files: file_rows.into_iter().map(Into::into).collect(),
        })
    }

    pub async fn save_saved_sql_folder(&self, folder: &SavedSqlFolder) -> Result<(), String> {
        sqlx::query(
            "INSERT INTO saved_sql_folders (id, connection_id, name, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET \
             connection_id = excluded.connection_id, \
             name = excluded.name, \
             updated_at = excluded.updated_at",
        )
        .bind(&folder.id)
        .bind(&folder.connection_id)
        .bind(&folder.name)
        .bind(&folder.created_at)
        .bind(&folder.updated_at)
        .execute(&self.db)
        .await
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub async fn delete_saved_sql_folder(&self, id: &str) -> Result<(), String> {
        let mut tx = self.db.begin().await.map_err(|e| e.to_string())?;
        sqlx::query("DELETE FROM saved_sql_files WHERE folder_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        sqlx::query("DELETE FROM saved_sql_folders WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        tx.commit().await.map_err(|e| e.to_string())?;
        Ok(())
    }

    pub async fn save_saved_sql_file(&self, file: &SavedSqlFile) -> Result<(), String> {
        sqlx::query(
            "INSERT INTO saved_sql_files \
             (id, connection_id, folder_id, name, database_name, schema_name, sql_text, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET \
             connection_id = excluded.connection_id, \
             folder_id = excluded.folder_id, \
             name = excluded.name, \
             database_name = excluded.database_name, \
             schema_name = excluded.schema_name, \
             sql_text = excluded.sql_text, \
             updated_at = excluded.updated_at",
        )
        .bind(&file.id)
        .bind(&file.connection_id)
        .bind(&file.folder_id)
        .bind(&file.name)
        .bind(&file.database)
        .bind(&file.schema)
        .bind(&file.sql)
        .bind(&file.created_at)
        .bind(&file.updated_at)
        .execute(&self.db)
        .await
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub async fn delete_saved_sql_file(&self, id: &str) -> Result<(), String> {
        sqlx::query("DELETE FROM saved_sql_files WHERE id = ?")
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Secrets
// ---------------------------------------------------------------------------

impl Storage {
    pub async fn get_secret(&self, connection_id: &str, key: &str) -> Result<Option<String>, String> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT secret FROM connection_secrets WHERE connection_id = ? AND key = ?")
                .bind(connection_id)
                .bind(key)
                .fetch_optional(&self.db)
                .await
                .map_err(|e| e.to_string())?;
        Ok(row.map(|(s,)| s))
    }

    pub async fn set_secret(&self, connection_id: &str, key: &str, secret: &str) -> Result<(), String> {
        sqlx::query(
            "INSERT OR REPLACE INTO connection_secrets (connection_id, key, secret) \
             VALUES (?, ?, ?)",
        )
        .bind(connection_id)
        .bind(key)
        .bind(secret)
        .execute(&self.db)
        .await
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub async fn delete_secret(&self, connection_id: &str, key: &str) -> Result<(), String> {
        sqlx::query("DELETE FROM connection_secrets WHERE connection_id = ? AND key = ?")
            .bind(connection_id)
            .bind(key)
            .execute(&self.db)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

impl Storage {
    pub async fn save_sidebar_layout(&self, layout: &serde_json::Value) -> Result<(), String> {
        let json = serde_json::to_string(layout).map_err(|e| e.to_string())?;
        sqlx::query("INSERT OR REPLACE INTO sidebar_layout (id, layout_json) VALUES (1, ?)")
            .bind(&json)
            .execute(&self.db)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub async fn load_sidebar_layout(&self) -> Result<Option<serde_json::Value>, String> {
        let row: Option<(String,)> = sqlx::query_as("SELECT layout_json FROM sidebar_layout WHERE id = 1")
            .fetch_optional(&self.db)
            .await
            .map_err(|e| e.to_string())?;
        match row {
            Some((json,)) => serde_json::from_str(&json).map(Some).map_err(|e| e.to_string()),
            None => Ok(None),
        }
    }
}

// ---------------------------------------------------------------------------
// Schema cache
// ---------------------------------------------------------------------------

impl Storage {
    pub async fn save_schema_cache(&self, cache_key: &str, payload: &serde_json::Value) -> Result<(), String> {
        let json = serde_json::to_string(payload).map_err(|e| e.to_string())?;
        sqlx::query(
            "INSERT OR REPLACE INTO schema_cache (cache_key, payload_json, updated_at) \
             VALUES (?, ?, datetime('now'))",
        )
        .bind(cache_key)
        .bind(&json)
        .execute(&self.db)
        .await
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub async fn load_schema_cache(&self, cache_key: &str) -> Result<Option<serde_json::Value>, String> {
        let row: Option<(String,)> = sqlx::query_as("SELECT payload_json FROM schema_cache WHERE cache_key = ?")
            .bind(cache_key)
            .fetch_optional(&self.db)
            .await
            .map_err(|e| e.to_string())?;
        match row {
            Some((json,)) => serde_json::from_str(&json).map(Some).map_err(|e| e.to_string()),
            None => Ok(None),
        }
    }

    pub async fn delete_schema_cache_prefix(&self, prefix: &str) -> Result<(), String> {
        sqlx::query("DELETE FROM schema_cache WHERE cache_key = ? OR substr(cache_key, 1, ?) = ?")
            .bind(prefix)
            .bind(prefix.len() as i64)
            .bind(prefix)
            .execute(&self.db)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// JSON migration
// ---------------------------------------------------------------------------

impl Storage {
    pub async fn migrate_from_json(&self, data_dir: &Path) -> Result<(), String> {
        self.migrate_connections_json(data_dir).await?;
        self.migrate_secrets_json(data_dir).await?;
        self.migrate_history_json(data_dir).await?;
        self.migrate_ai_config_json(data_dir).await?;
        self.migrate_ai_conversations_json(data_dir).await?;
        self.migrate_sidebar_layout_json(data_dir).await?;
        Ok(())
    }

    async fn migrate_connections_json(&self, data_dir: &Path) -> Result<(), String> {
        let path = data_dir.join("connections.json");
        if !path.exists() {
            return Ok(());
        }
        let json = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let configs: Vec<ConnectionConfig> = serde_json::from_str(&json).unwrap_or_default();
        for config in &configs {
            let config_json = serde_json::to_string(config).map_err(|e| e.to_string())?;
            sqlx::query("INSERT OR IGNORE INTO connections (id, config_json) VALUES (?, ?)")
                .bind(&config.id)
                .bind(&config_json)
                .execute(&self.db)
                .await
                .map_err(|e| e.to_string())?;
        }
        std::fs::rename(&path, data_dir.join("connections.json.bak")).ok();
        Ok(())
    }

    async fn migrate_secrets_json(&self, data_dir: &Path) -> Result<(), String> {
        let path = data_dir.join("secrets.json");
        if !path.exists() {
            return Ok(());
        }
        let json = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let secrets: std::collections::HashMap<String, String> = serde_json::from_str(&json).unwrap_or_default();
        for (key, secret) in &secrets {
            // key format: "connection:{id}:{field}"
            let parts: Vec<&str> = key.splitn(3, ':').collect();
            if parts.len() == 3 && parts[0] == "connection" {
                sqlx::query(
                    "INSERT OR IGNORE INTO connection_secrets \
                     (connection_id, key, secret) VALUES (?, ?, ?)",
                )
                .bind(parts[1])
                .bind(parts[2])
                .bind(secret)
                .execute(&self.db)
                .await
                .map_err(|e| e.to_string())?;
            }
        }
        std::fs::rename(&path, data_dir.join("secrets.json.bak")).ok();
        Ok(())
    }

    async fn migrate_history_json(&self, data_dir: &Path) -> Result<(), String> {
        let path = data_dir.join("query_history.json");
        if !path.exists() {
            return Ok(());
        }
        let json = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let entries: Vec<HistoryEntry> = serde_json::from_str(&json).unwrap_or_default();
        for entry in &entries {
            sqlx::query(
                "INSERT OR IGNORE INTO history \
                 (id, connection_name, database, sql_text, executed_at, \
                  execution_time_ms, success, error, activity_kind, connection_id, operation, target, \
                  affected_rows, rollback_sql, details_json) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&entry.id)
            .bind(&entry.connection_name)
            .bind(&entry.database)
            .bind(&entry.sql)
            .bind(&entry.executed_at)
            .bind(entry.execution_time_ms as i64)
            .bind(entry.success)
            .bind(&entry.error)
            .bind(&entry.activity_kind)
            .bind(&entry.connection_id)
            .bind(&entry.operation)
            .bind(&entry.target)
            .bind(entry.affected_rows)
            .bind(&entry.rollback_sql)
            .bind(&entry.details_json)
            .execute(&self.db)
            .await
            .map_err(|e| e.to_string())?;
        }
        std::fs::rename(&path, data_dir.join("query_history.json.bak")).ok();
        Ok(())
    }

    async fn migrate_ai_config_json(&self, data_dir: &Path) -> Result<(), String> {
        let path = data_dir.join("ai_config.json");
        if !path.exists() {
            return Ok(());
        }
        let json = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        // Only migrate if the table is empty
        let count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM ai_config").fetch_one(&self.db).await.map_err(|e| e.to_string())?;
        if count.0 == 0 {
            sqlx::query("INSERT OR IGNORE INTO ai_config (id, config_json) VALUES (1, ?)")
                .bind(&json)
                .execute(&self.db)
                .await
                .map_err(|e| e.to_string())?;
        }
        std::fs::rename(&path, data_dir.join("ai_config.json.bak")).ok();
        Ok(())
    }

    async fn migrate_ai_conversations_json(&self, data_dir: &Path) -> Result<(), String> {
        let path = data_dir.join("ai_conversations.json");
        if !path.exists() {
            return Ok(());
        }
        let json = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let conversations: Vec<AiConversation> = serde_json::from_str(&json).unwrap_or_default();
        for conv in &conversations {
            let messages_json = serde_json::to_string(&conv.messages).map_err(|e| e.to_string())?;
            sqlx::query(
                "INSERT OR IGNORE INTO ai_conversations \
                 (id, title, connection_name, database, messages_json, \
                  created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&conv.id)
            .bind(&conv.title)
            .bind(&conv.connection_name)
            .bind(&conv.database)
            .bind(&messages_json)
            .bind(&conv.created_at)
            .bind(&conv.updated_at)
            .execute(&self.db)
            .await
            .map_err(|e| e.to_string())?;
        }
        std::fs::rename(&path, data_dir.join("ai_conversations.json.bak")).ok();
        Ok(())
    }

    async fn migrate_sidebar_layout_json(&self, data_dir: &Path) -> Result<(), String> {
        let path = data_dir.join("sidebar_layout.json");
        if !path.exists() {
            return Ok(());
        }
        let json = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sidebar_layout")
            .fetch_one(&self.db)
            .await
            .map_err(|e| e.to_string())?;
        if count.0 == 0 {
            sqlx::query("INSERT OR IGNORE INTO sidebar_layout (id, layout_json) VALUES (1, ?)")
                .bind(&json)
                .execute(&self.db)
                .await
                .map_err(|e| e.to_string())?;
        }
        std::fs::rename(&path, data_dir.join("sidebar_layout.json.bak")).ok();
        Ok(())
    }
}

#[cfg(test)]
mod handoff_tests {
    use super::*;
    use crate::handoff::{HandoffItem, HandoffStatus};
    use crate::sql_safety::{OperationClass, RiskLevel};

    async fn open_temp_storage() -> Storage {
        let path = std::env::temp_dir().join(format!("dbx-handoff-test-{}.db", uuid::Uuid::new_v4()));
        Storage::open(&path).await.unwrap()
    }

    fn queued_handoff(title: &str) -> HandoffItem {
        HandoffItem::queued(
            "prod-main-id".to_string(),
            "prod-main".to_string(),
            Some("app".to_string()),
            title.to_string(),
            Some("review write".to_string()),
            "UPDATE users SET active = 0".to_string(),
            OperationClass::Write,
            RiskLevel::High,
            true,
        )
    }

    #[tokio::test]
    async fn save_handoff_loads_pending_records_in_created_order() {
        let storage = open_temp_storage().await;
        let first = queued_handoff("first");
        let mut second = queued_handoff("second");
        second.created_at = first.created_at + chrono::Duration::seconds(1);
        second.status = HandoffStatus::Shown;

        storage.save_handoff(&second).await.unwrap();
        storage.save_handoff(&first).await.unwrap();

        let loaded = storage.load_pending_handoffs().await.unwrap();

        assert_eq!(loaded.iter().map(|item| item.title.as_str()).collect::<Vec<_>>(), vec!["first", "second"]);
        assert_eq!(loaded[0].status, HandoffStatus::Queued);
        assert_eq!(loaded[1].status, HandoffStatus::Shown);
        assert_eq!(loaded[0].operation_class, OperationClass::Write);
        assert!(loaded[0].is_production);
    }

    #[tokio::test]
    async fn load_pending_handoffs_keeps_fifo_order_for_matching_created_at() {
        let storage = open_temp_storage().await;
        let first = queued_handoff("first");
        let mut second = queued_handoff("second");
        second.created_at = first.created_at;

        storage.save_handoff(&first).await.unwrap();
        storage.save_handoff(&second).await.unwrap();

        let loaded = storage.load_pending_handoffs().await.unwrap();

        assert_eq!(loaded.iter().map(|item| item.title.as_str()).collect::<Vec<_>>(), vec!["first", "second"]);
    }

    #[tokio::test]
    async fn handoffs_table_has_stable_autoincrement_sequence() {
        let storage = open_temp_storage().await;

        let columns: Vec<(String,)> =
            sqlx::query_as("SELECT name FROM pragma_table_info('handoffs')").fetch_all(&storage.db).await.unwrap();

        assert!(columns.iter().any(|(name,)| name == "seq"));
    }

    #[tokio::test]
    async fn handoff_item_serializes_connection_id_and_display_name() {
        let item = queued_handoff("serialize");

        let value = serde_json::to_value(&item).unwrap();

        assert_eq!(item.connection_id, "prod-main-id");
        assert_eq!(value["connectionId"], "prod-main-id");
        assert_eq!(value["connectionName"], "prod-main");
    }

    #[tokio::test]
    async fn load_pending_handoffs_excludes_terminal_statuses() {
        let storage = open_temp_storage().await;
        let queued = queued_handoff("queued");
        let mut executed = queued_handoff("executed");
        executed.status = HandoffStatus::Executed;

        storage.save_handoff(&queued).await.unwrap();
        storage.save_handoff(&executed).await.unwrap();

        let loaded = storage.load_pending_handoffs().await.unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, queued.id);
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn persist_secret_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    connection_id: &str,
    key: &str,
    secret: &str,
) -> Result<(), String> {
    if secret.is_empty() {
        sqlx::query("DELETE FROM connection_secrets WHERE connection_id = ? AND key = ?")
            .bind(connection_id)
            .bind(key)
            .execute(&mut **tx)
            .await
            .map_err(|e| e.to_string())?;
    } else {
        sqlx::query(
            "INSERT OR REPLACE INTO connection_secrets \
             (connection_id, key, secret) VALUES (?, ?, ?)",
        )
        .bind(connection_id)
        .bind(key)
        .bind(secret)
        .execute(&mut **tx)
        .await
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}
