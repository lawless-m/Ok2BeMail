use crate::error::Result;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Email {
    pub id: String,
    pub subject: Option<String>,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub sender_address: String,
    pub sender_name: Option<String>,
    pub received_at: DateTime<Utc>,
    pub folder_id: Option<String>,
    pub has_attachments: bool,
    pub is_read: bool,
    pub importance: Option<String>,
    pub fetched_at: DateTime<Utc>,
    pub embedding: Option<Vec<f32>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Label {
    pub email_id: String,
    pub category: String,
    pub importance: i32,
    pub source: String,
    pub confidence: Option<f64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct EmailWithLabel {
    pub email: Email,
    pub label: Option<Label>,
}

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();

        // Create parent directories if needed
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(path)?;

        // Set pragmas for better performance
        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA foreign_keys = ON;
            PRAGMA busy_timeout = 5000;
            ",
        )?;

        let db = Database { conn };
        db.init_schema()?;

        // Set restrictive permissions on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            let _ = std::fs::set_permissions(path, perms);
        }

        Ok(db)
    }

    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            -- Core email storage
            CREATE TABLE IF NOT EXISTS emails (
                id TEXT PRIMARY KEY,
                subject TEXT,
                body_text TEXT,
                body_html TEXT,
                sender_address TEXT NOT NULL,
                sender_name TEXT,
                received_at TEXT NOT NULL,
                folder_id TEXT,
                has_attachments INTEGER DEFAULT 0,
                is_read INTEGER DEFAULT 0,
                importance TEXT,
                fetched_at TEXT NOT NULL,
                embedding BLOB,
                created_at TEXT DEFAULT CURRENT_TIMESTAMP
            );

            -- Classification labels
            CREATE TABLE IF NOT EXISTS labels (
                email_id TEXT PRIMARY KEY REFERENCES emails(id) ON DELETE CASCADE,
                category TEXT NOT NULL,
                importance INTEGER DEFAULT 0,
                source TEXT NOT NULL,
                confidence REAL,
                created_at TEXT DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT DEFAULT CURRENT_TIMESTAMP
            );

            -- Sync state
            CREATE TABLE IF NOT EXISTS sync_state (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT DEFAULT CURRENT_TIMESTAMP
            );

            -- Categories for user-defined classification types
            CREATE TABLE IF NOT EXISTS categories (
                name TEXT PRIMARY KEY,
                description TEXT,
                created_at TEXT DEFAULT CURRENT_TIMESTAMP
            );

            -- Insert default categories
            INSERT OR IGNORE INTO categories (name, description) VALUES
                ('hr', 'Human resources communications, policies, benefits'),
                ('system_alert', 'Automated system notifications, monitoring alerts, CI/CD failures'),
                ('system_ok', 'Routine system status, successful deployments, all-clear notices'),
                ('external', 'Emails from outside the organisation'),
                ('internal', 'General internal communications'),
                ('junk', 'Newsletters, marketing, low-priority automated emails');

            -- Indexes
            CREATE INDEX IF NOT EXISTS idx_emails_received ON emails(received_at DESC);
            CREATE INDEX IF NOT EXISTS idx_emails_sender ON emails(sender_address);
            CREATE INDEX IF NOT EXISTS idx_labels_category ON labels(category);
            CREATE INDEX IF NOT EXISTS idx_labels_source ON labels(source);
            ",
        )?;

        Ok(())
    }

    // Email operations
    pub fn insert_email(&self, email: &Email) -> Result<()> {
        let embedding_blob = email.embedding.as_ref().map(|v| {
            v.iter()
                .flat_map(|f| f.to_le_bytes())
                .collect::<Vec<u8>>()
        });

        self.conn.execute(
            "INSERT OR REPLACE INTO emails (
                id, subject, body_text, body_html, sender_address, sender_name,
                received_at, folder_id, has_attachments, is_read, importance,
                fetched_at, embedding
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                email.id,
                email.subject,
                email.body_text,
                email.body_html,
                email.sender_address,
                email.sender_name,
                email.received_at.to_rfc3339(),
                email.folder_id,
                email.has_attachments as i32,
                email.is_read as i32,
                email.importance,
                email.fetched_at.to_rfc3339(),
                embedding_blob,
            ],
        )?;

        Ok(())
    }

    pub fn get_email(&self, id: &str) -> Result<Option<Email>> {
        let result = self
            .conn
            .query_row(
                "SELECT id, subject, body_text, body_html, sender_address, sender_name,
                        received_at, folder_id, has_attachments, is_read, importance,
                        fetched_at, embedding
                 FROM emails WHERE id = ?1",
                params![id],
                |row| {
                    let embedding_blob: Option<Vec<u8>> = row.get(12)?;
                    let embedding = embedding_blob.map(|blob| {
                        blob.chunks(4)
                            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                            .collect()
                    });

                    Ok(Email {
                        id: row.get(0)?,
                        subject: row.get(1)?,
                        body_text: row.get(2)?,
                        body_html: row.get(3)?,
                        sender_address: row.get(4)?,
                        sender_name: row.get(5)?,
                        received_at: parse_datetime(row.get::<_, String>(6)?),
                        folder_id: row.get(7)?,
                        has_attachments: row.get::<_, i32>(8)? != 0,
                        is_read: row.get::<_, i32>(9)? != 0,
                        importance: row.get(10)?,
                        fetched_at: parse_datetime(row.get::<_, String>(11)?),
                        embedding,
                    })
                },
            )
            .optional()?;

        Ok(result)
    }

    pub fn update_embedding(&self, email_id: &str, embedding: &[f32]) -> Result<()> {
        let blob: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();

        self.conn.execute(
            "UPDATE emails SET embedding = ?1 WHERE id = ?2",
            params![blob, email_id],
        )?;

        Ok(())
    }

    pub fn get_emails_without_embedding(&self, limit: usize) -> Result<Vec<Email>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, subject, body_text, body_html, sender_address, sender_name,
                    received_at, folder_id, has_attachments, is_read, importance,
                    fetched_at, embedding
             FROM emails
             WHERE embedding IS NULL
             ORDER BY received_at DESC
             LIMIT ?1",
        )?;

        let emails = stmt
            .query_map(params![limit as i64], |row| {
                Ok(Email {
                    id: row.get(0)?,
                    subject: row.get(1)?,
                    body_text: row.get(2)?,
                    body_html: row.get(3)?,
                    sender_address: row.get(4)?,
                    sender_name: row.get(5)?,
                    received_at: parse_datetime(row.get::<_, String>(6)?),
                    folder_id: row.get(7)?,
                    has_attachments: row.get::<_, i32>(8)? != 0,
                    is_read: row.get::<_, i32>(9)? != 0,
                    importance: row.get(10)?,
                    fetched_at: parse_datetime(row.get::<_, String>(11)?),
                    embedding: None,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(emails)
    }

    pub fn get_emails_without_label(&self, limit: usize) -> Result<Vec<Email>> {
        let mut stmt = self.conn.prepare(
            "SELECT e.id, e.subject, e.body_text, e.body_html, e.sender_address, e.sender_name,
                    e.received_at, e.folder_id, e.has_attachments, e.is_read, e.importance,
                    e.fetched_at, e.embedding
             FROM emails e
             LEFT JOIN labels l ON e.id = l.email_id
             WHERE l.email_id IS NULL AND e.embedding IS NOT NULL
             ORDER BY e.received_at DESC
             LIMIT ?1",
        )?;

        let emails = stmt
            .query_map(params![limit as i64], |row| {
                let embedding_blob: Option<Vec<u8>> = row.get(12)?;
                let embedding = embedding_blob.map(|blob| {
                    blob.chunks(4)
                        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                        .collect()
                });

                Ok(Email {
                    id: row.get(0)?,
                    subject: row.get(1)?,
                    body_text: row.get(2)?,
                    body_html: row.get(3)?,
                    sender_address: row.get(4)?,
                    sender_name: row.get(5)?,
                    received_at: parse_datetime(row.get::<_, String>(6)?),
                    folder_id: row.get(7)?,
                    has_attachments: row.get::<_, i32>(8)? != 0,
                    is_read: row.get::<_, i32>(9)? != 0,
                    importance: row.get(10)?,
                    fetched_at: parse_datetime(row.get::<_, String>(11)?),
                    embedding,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(emails)
    }

    // Label operations
    pub fn insert_label(&self, label: &Label) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO labels (
                email_id, category, importance, source, confidence, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                label.email_id,
                label.category,
                label.importance,
                label.source,
                label.confidence,
                label.created_at.to_rfc3339(),
                label.updated_at.to_rfc3339(),
            ],
        )?;

        Ok(())
    }

    pub fn get_label(&self, email_id: &str) -> Result<Option<Label>> {
        let result = self
            .conn
            .query_row(
                "SELECT email_id, category, importance, source, confidence, created_at, updated_at
                 FROM labels WHERE email_id = ?1",
                params![email_id],
                |row| {
                    Ok(Label {
                        email_id: row.get(0)?,
                        category: row.get(1)?,
                        importance: row.get(2)?,
                        source: row.get(3)?,
                        confidence: row.get(4)?,
                        created_at: parse_datetime(row.get::<_, String>(5)?),
                        updated_at: parse_datetime(row.get::<_, String>(6)?),
                    })
                },
            )
            .optional()?;

        Ok(result)
    }

    pub fn correct_label(
        &self,
        email_id: &str,
        category: &str,
        importance: i32,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();

        self.conn.execute(
            "INSERT INTO labels (email_id, category, importance, source, confidence, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'user', NULL, ?4, ?4)
             ON CONFLICT(email_id) DO UPDATE SET
                category = ?2,
                importance = ?3,
                source = 'user',
                confidence = NULL,
                updated_at = ?4",
            params![email_id, category, importance, now],
        )?;

        Ok(())
    }

    pub fn verify_label(&self, email_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();

        self.conn.execute(
            "UPDATE labels SET source = 'user', updated_at = ?1 WHERE email_id = ?2",
            params![now, email_id],
        )?;

        Ok(())
    }

    // Get user-verified emails for few-shot learning (with embeddings)
    pub fn get_verified_emails_with_embeddings(&self, limit: usize) -> Result<Vec<EmailWithLabel>> {
        let mut stmt = self.conn.prepare(
            "SELECT e.id, e.subject, e.body_text, e.body_html, e.sender_address, e.sender_name,
                    e.received_at, e.folder_id, e.has_attachments, e.is_read, e.importance,
                    e.fetched_at, e.embedding,
                    l.email_id, l.category, l.importance, l.source, l.confidence, l.created_at, l.updated_at
             FROM emails e
             JOIN labels l ON e.id = l.email_id
             WHERE l.source = 'user' AND e.embedding IS NOT NULL
             ORDER BY l.updated_at DESC
             LIMIT ?1",
        )?;

        let results = stmt
            .query_map(params![limit as i64], |row| {
                let embedding_blob: Option<Vec<u8>> = row.get(12)?;
                let embedding = embedding_blob.map(|blob| {
                    blob.chunks(4)
                        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                        .collect()
                });

                Ok(EmailWithLabel {
                    email: Email {
                        id: row.get(0)?,
                        subject: row.get(1)?,
                        body_text: row.get(2)?,
                        body_html: row.get(3)?,
                        sender_address: row.get(4)?,
                        sender_name: row.get(5)?,
                        received_at: parse_datetime(row.get::<_, String>(6)?),
                        folder_id: row.get(7)?,
                        has_attachments: row.get::<_, i32>(8)? != 0,
                        is_read: row.get::<_, i32>(9)? != 0,
                        importance: row.get(10)?,
                        fetched_at: parse_datetime(row.get::<_, String>(11)?),
                        embedding,
                    },
                    label: Some(Label {
                        email_id: row.get(13)?,
                        category: row.get(14)?,
                        importance: row.get(15)?,
                        source: row.get(16)?,
                        confidence: row.get(17)?,
                        created_at: parse_datetime(row.get::<_, String>(18)?),
                        updated_at: parse_datetime(row.get::<_, String>(19)?),
                    }),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(results)
    }

    // Sync state operations
    pub fn get_sync_state(&self, key: &str) -> Result<Option<String>> {
        let result = self
            .conn
            .query_row(
                "SELECT value FROM sync_state WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?;

        Ok(result)
    }

    pub fn set_sync_state(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO sync_state (key, value, updated_at)
             VALUES (?1, ?2, CURRENT_TIMESTAMP)",
            params![key, value],
        )?;

        Ok(())
    }

    // Category operations
    pub fn add_category(&self, name: &str, description: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO categories (name, description) VALUES (?1, ?2)",
            params![name, description],
        )?;

        Ok(())
    }

    pub fn list_categories(&self) -> Result<Vec<(String, Option<String>)>> {
        let mut stmt = self.conn.prepare("SELECT name, description FROM categories ORDER BY name")?;

        let categories = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(categories)
    }

    // List emails with labels for review
    pub fn list_emails_with_labels(
        &self,
        unverified_only: bool,
        category: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EmailWithLabel>> {
        let mut query = String::from(
            "SELECT e.id, e.subject, e.body_text, e.body_html, e.sender_address, e.sender_name,
                    e.received_at, e.folder_id, e.has_attachments, e.is_read, e.importance,
                    e.fetched_at, e.embedding,
                    l.email_id, l.category, l.importance, l.source, l.confidence, l.created_at, l.updated_at
             FROM emails e
             LEFT JOIN labels l ON e.id = l.email_id
             WHERE 1=1",
        );

        if unverified_only {
            query.push_str(" AND (l.source IS NULL OR l.source = 'model')");
        }

        if category.is_some() {
            query.push_str(" AND l.category = ?1");
        }

        query.push_str(" ORDER BY e.received_at DESC LIMIT ?2");

        let mut stmt = self.conn.prepare(&query)?;

        let results = if let Some(cat) = category {
            stmt.query_map(params![cat, limit as i64], map_email_with_label)?
        } else {
            stmt.query_map(params![limit as i64], map_email_with_label)?
        };

        let emails = results.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(emails)
    }

    pub fn email_count(&self) -> Result<i64> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM emails", [], |row| row.get(0))?;
        Ok(count)
    }

    pub fn labeled_count(&self) -> Result<i64> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM labels", [], |row| row.get(0))?;
        Ok(count)
    }

    pub fn user_verified_count(&self) -> Result<i64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM labels WHERE source = 'user'",
            [],
            |row| row.get(0),
        )?;
        Ok(count)
    }
}

fn map_email_with_label(row: &rusqlite::Row) -> rusqlite::Result<EmailWithLabel> {
    let embedding_blob: Option<Vec<u8>> = row.get(12)?;
    let embedding = embedding_blob.map(|blob| {
        blob.chunks(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect()
    });

    let label_email_id: Option<String> = row.get(13)?;
    let label = if label_email_id.is_some() {
        Some(Label {
            email_id: row.get(13)?,
            category: row.get(14)?,
            importance: row.get(15)?,
            source: row.get(16)?,
            confidence: row.get(17)?,
            created_at: parse_datetime(row.get::<_, String>(18)?),
            updated_at: parse_datetime(row.get::<_, String>(19)?),
        })
    } else {
        None
    };

    Ok(EmailWithLabel {
        email: Email {
            id: row.get(0)?,
            subject: row.get(1)?,
            body_text: row.get(2)?,
            body_html: row.get(3)?,
            sender_address: row.get(4)?,
            sender_name: row.get(5)?,
            received_at: parse_datetime(row.get::<_, String>(6)?),
            folder_id: row.get(7)?,
            has_attachments: row.get::<_, i32>(8)? != 0,
            is_read: row.get::<_, i32>(9)? != 0,
            importance: row.get(10)?,
            fetched_at: parse_datetime(row.get::<_, String>(11)?),
            embedding,
        },
        label,
    })
}

fn parse_datetime(s: String) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&s)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}
