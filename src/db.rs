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
            query.push_str(" ORDER BY e.received_at DESC LIMIT ?2");
        } else {
            query.push_str(" ORDER BY e.received_at DESC LIMIT ?1");
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_email(id: &str, subject: &str, sender: &str) -> Email {
        Email {
            id: id.to_string(),
            subject: Some(subject.to_string()),
            body_text: Some("Test body content".to_string()),
            body_html: Some("<p>Test body content</p>".to_string()),
            sender_address: sender.to_string(),
            sender_name: Some("Test Sender".to_string()),
            received_at: Utc::now(),
            folder_id: Some("inbox".to_string()),
            has_attachments: false,
            is_read: false,
            importance: Some("normal".to_string()),
            fetched_at: Utc::now(),
            embedding: None,
        }
    }

    fn make_test_label(email_id: &str, category: &str, importance: i32, source: &str) -> Label {
        Label {
            email_id: email_id.to_string(),
            category: category.to_string(),
            importance,
            source: source.to_string(),
            confidence: Some(0.85),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn create_test_db() -> Database {
        Database::open(":memory:").expect("Failed to create in-memory database")
    }

    #[test]
    fn test_database_creation() {
        let db = create_test_db();
        // Should have default categories
        let categories = db.list_categories().expect("Failed to list categories");
        assert!(!categories.is_empty(), "Default categories should be created");

        let category_names: Vec<_> = categories.iter().map(|(name, _)| name.as_str()).collect();
        assert!(category_names.contains(&"hr"));
        assert!(category_names.contains(&"system_alert"));
        assert!(category_names.contains(&"system_ok"));
        assert!(category_names.contains(&"external"));
        assert!(category_names.contains(&"internal"));
        assert!(category_names.contains(&"junk"));
    }

    #[test]
    fn test_insert_and_get_email() {
        let db = create_test_db();
        let email = make_test_email("email-123", "Test Subject", "test@example.com");

        db.insert_email(&email).expect("Failed to insert email");

        let retrieved = db.get_email("email-123").expect("Failed to get email");
        assert!(retrieved.is_some(), "Email should be retrievable");

        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.id, "email-123");
        assert_eq!(retrieved.subject, Some("Test Subject".to_string()));
        assert_eq!(retrieved.sender_address, "test@example.com");
        assert_eq!(retrieved.sender_name, Some("Test Sender".to_string()));
        assert!(!retrieved.has_attachments);
        assert!(!retrieved.is_read);
    }

    #[test]
    fn test_get_nonexistent_email() {
        let db = create_test_db();

        let result = db.get_email("nonexistent-id").expect("Query should not fail");
        assert!(result.is_none(), "Should return None for nonexistent email");
    }

    #[test]
    fn test_insert_email_with_embedding() {
        let db = create_test_db();
        let mut email = make_test_email("email-with-emb", "Embedded Email", "embed@example.com");
        email.embedding = Some(vec![0.1, 0.2, 0.3, 0.4]);

        db.insert_email(&email).expect("Failed to insert email with embedding");

        let retrieved = db.get_email("email-with-emb").expect("Failed to get email").unwrap();
        assert!(retrieved.embedding.is_some(), "Embedding should be retrieved");

        let emb = retrieved.embedding.unwrap();
        assert_eq!(emb.len(), 4);
        assert!((emb[0] - 0.1).abs() < 0.0001);
        assert!((emb[1] - 0.2).abs() < 0.0001);
        assert!((emb[2] - 0.3).abs() < 0.0001);
        assert!((emb[3] - 0.4).abs() < 0.0001);
    }

    #[test]
    fn test_update_embedding() {
        let db = create_test_db();
        let email = make_test_email("email-456", "Update Embedding", "update@example.com");

        db.insert_email(&email).expect("Failed to insert email");

        // Initially no embedding
        let retrieved = db.get_email("email-456").expect("Query failed").unwrap();
        assert!(retrieved.embedding.is_none());

        // Update embedding
        let new_embedding = vec![1.0, 2.0, 3.0];
        db.update_embedding("email-456", &new_embedding).expect("Failed to update embedding");

        // Verify update
        let retrieved = db.get_email("email-456").expect("Query failed").unwrap();
        assert!(retrieved.embedding.is_some());
        let emb = retrieved.embedding.unwrap();
        assert_eq!(emb.len(), 3);
        assert!((emb[0] - 1.0).abs() < 0.0001);
    }

    #[test]
    fn test_insert_and_get_label() {
        let db = create_test_db();
        let email = make_test_email("email-for-label", "Labeled Email", "label@example.com");
        let label = make_test_label("email-for-label", "external", 3, "model");

        db.insert_email(&email).expect("Failed to insert email");
        db.insert_label(&label).expect("Failed to insert label");

        let retrieved = db.get_label("email-for-label").expect("Failed to get label");
        assert!(retrieved.is_some());

        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.email_id, "email-for-label");
        assert_eq!(retrieved.category, "external");
        assert_eq!(retrieved.importance, 3);
        assert_eq!(retrieved.source, "model");
        assert!((retrieved.confidence.unwrap() - 0.85).abs() < 0.001);
    }

    #[test]
    fn test_correct_label() {
        let db = create_test_db();
        let email = make_test_email("email-to-correct", "Correct Me", "correct@example.com");
        let label = make_test_label("email-to-correct", "junk", 0, "model");

        db.insert_email(&email).expect("Failed to insert email");
        db.insert_label(&label).expect("Failed to insert label");

        // Correct the label
        db.correct_label("email-to-correct", "internal", 2).expect("Failed to correct label");

        let corrected = db.get_label("email-to-correct").expect("Failed to get label").unwrap();
        assert_eq!(corrected.category, "internal");
        assert_eq!(corrected.importance, 2);
        assert_eq!(corrected.source, "user");
        assert!(corrected.confidence.is_none(), "User corrections should have no confidence");
    }

    #[test]
    fn test_verify_label() {
        let db = create_test_db();
        let email = make_test_email("email-to-verify", "Verify Me", "verify@example.com");
        let label = make_test_label("email-to-verify", "hr", 2, "model");

        db.insert_email(&email).expect("Failed to insert email");
        db.insert_label(&label).expect("Failed to insert label");

        // Verify the label
        db.verify_label("email-to-verify").expect("Failed to verify label");

        let verified = db.get_label("email-to-verify").expect("Failed to get label").unwrap();
        assert_eq!(verified.source, "user", "Verified label should have user source");
        assert_eq!(verified.category, "hr", "Category should remain unchanged");
        assert_eq!(verified.importance, 2, "Importance should remain unchanged");
    }

    #[test]
    fn test_sync_state() {
        let db = create_test_db();

        // Initially no sync state
        let state = db.get_sync_state("delta_link").expect("Query failed");
        assert!(state.is_none());

        // Set sync state
        db.set_sync_state("delta_link", "https://example.com/delta?token=abc123")
            .expect("Failed to set sync state");

        // Retrieve sync state
        let state = db.get_sync_state("delta_link").expect("Query failed");
        assert!(state.is_some());
        assert_eq!(state.unwrap(), "https://example.com/delta?token=abc123");

        // Update sync state
        db.set_sync_state("delta_link", "https://example.com/delta?token=xyz789")
            .expect("Failed to update sync state");

        let state = db.get_sync_state("delta_link").expect("Query failed");
        assert_eq!(state.unwrap(), "https://example.com/delta?token=xyz789");
    }

    #[test]
    fn test_add_custom_category() {
        let db = create_test_db();

        db.add_category("urgent", "Time-sensitive important emails")
            .expect("Failed to add category");

        let categories = db.list_categories().expect("Failed to list categories");
        let urgent_cat = categories.iter().find(|(name, _)| name == "urgent");

        assert!(urgent_cat.is_some());
        assert_eq!(urgent_cat.unwrap().1, Some("Time-sensitive important emails".to_string()));
    }

    #[test]
    fn test_email_count() {
        let db = create_test_db();

        assert_eq!(db.email_count().unwrap(), 0);

        db.insert_email(&make_test_email("e1", "Email 1", "a@example.com")).unwrap();
        assert_eq!(db.email_count().unwrap(), 1);

        db.insert_email(&make_test_email("e2", "Email 2", "b@example.com")).unwrap();
        assert_eq!(db.email_count().unwrap(), 2);

        db.insert_email(&make_test_email("e3", "Email 3", "c@example.com")).unwrap();
        assert_eq!(db.email_count().unwrap(), 3);
    }

    #[test]
    fn test_labeled_count() {
        let db = create_test_db();

        db.insert_email(&make_test_email("e1", "Email 1", "a@example.com")).unwrap();
        db.insert_email(&make_test_email("e2", "Email 2", "b@example.com")).unwrap();

        assert_eq!(db.labeled_count().unwrap(), 0);

        db.insert_label(&make_test_label("e1", "internal", 2, "model")).unwrap();
        assert_eq!(db.labeled_count().unwrap(), 1);

        db.insert_label(&make_test_label("e2", "external", 1, "user")).unwrap();
        assert_eq!(db.labeled_count().unwrap(), 2);
    }

    #[test]
    fn test_user_verified_count() {
        let db = create_test_db();

        db.insert_email(&make_test_email("e1", "Email 1", "a@example.com")).unwrap();
        db.insert_email(&make_test_email("e2", "Email 2", "b@example.com")).unwrap();
        db.insert_email(&make_test_email("e3", "Email 3", "c@example.com")).unwrap();

        db.insert_label(&make_test_label("e1", "internal", 2, "model")).unwrap();
        db.insert_label(&make_test_label("e2", "external", 1, "user")).unwrap();
        db.insert_label(&make_test_label("e3", "junk", 0, "model")).unwrap();

        assert_eq!(db.user_verified_count().unwrap(), 1);

        // Verify another label
        db.verify_label("e1").unwrap();
        assert_eq!(db.user_verified_count().unwrap(), 2);
    }

    #[test]
    fn test_get_emails_without_embedding() {
        let db = create_test_db();

        // Insert email without embedding
        let mut email1 = make_test_email("e1", "No Embedding", "a@example.com");
        email1.embedding = None;
        db.insert_email(&email1).unwrap();

        // Insert email with embedding
        let mut email2 = make_test_email("e2", "Has Embedding", "b@example.com");
        email2.embedding = Some(vec![1.0, 2.0, 3.0]);
        db.insert_email(&email2).unwrap();

        let without_embedding = db.get_emails_without_embedding(10).unwrap();
        assert_eq!(without_embedding.len(), 1);
        assert_eq!(without_embedding[0].id, "e1");
    }

    #[test]
    fn test_get_emails_without_label() {
        let db = create_test_db();

        // Insert emails with embeddings
        let mut email1 = make_test_email("e1", "Unlabeled", "a@example.com");
        email1.embedding = Some(vec![1.0, 2.0, 3.0]);
        db.insert_email(&email1).unwrap();

        let mut email2 = make_test_email("e2", "Labeled", "b@example.com");
        email2.embedding = Some(vec![4.0, 5.0, 6.0]);
        db.insert_email(&email2).unwrap();

        // Label only the second email
        db.insert_label(&make_test_label("e2", "internal", 2, "model")).unwrap();

        let without_label = db.get_emails_without_label(10).unwrap();
        assert_eq!(without_label.len(), 1);
        assert_eq!(without_label[0].id, "e1");
    }

    #[test]
    fn test_get_verified_emails_with_embeddings() {
        let db = create_test_db();

        // Create emails with embeddings
        let mut email1 = make_test_email("e1", "User Verified", "a@example.com");
        email1.embedding = Some(vec![1.0, 2.0]);
        db.insert_email(&email1).unwrap();

        let mut email2 = make_test_email("e2", "Model Labeled", "b@example.com");
        email2.embedding = Some(vec![3.0, 4.0]);
        db.insert_email(&email2).unwrap();

        let mut email3 = make_test_email("e3", "User No Embedding", "c@example.com");
        email3.embedding = None;
        db.insert_email(&email3).unwrap();

        db.insert_label(&make_test_label("e1", "internal", 2, "user")).unwrap();
        db.insert_label(&make_test_label("e2", "external", 1, "model")).unwrap();
        db.insert_label(&make_test_label("e3", "junk", 0, "user")).unwrap();

        let verified = db.get_verified_emails_with_embeddings(10).unwrap();

        // Only e1 has both user verification AND embedding
        assert_eq!(verified.len(), 1);
        assert_eq!(verified[0].email.id, "e1");
        assert!(verified[0].label.is_some());
        assert_eq!(verified[0].label.as_ref().unwrap().source, "user");
    }

    #[test]
    fn test_list_emails_with_labels() {
        let db = create_test_db();

        db.insert_email(&make_test_email("e1", "Email 1", "a@example.com")).unwrap();
        db.insert_email(&make_test_email("e2", "Email 2", "b@example.com")).unwrap();
        db.insert_email(&make_test_email("e3", "Email 3", "c@example.com")).unwrap();

        db.insert_label(&make_test_label("e1", "internal", 2, "model")).unwrap();
        db.insert_label(&make_test_label("e2", "external", 1, "user")).unwrap();
        // e3 has no label

        // List all
        let all = db.list_emails_with_labels(false, None, 10).unwrap();
        assert_eq!(all.len(), 3);

        // List unverified only (model + unlabeled)
        let unverified = db.list_emails_with_labels(true, None, 10).unwrap();
        assert_eq!(unverified.len(), 2); // e1 (model) and e3 (no label)

        // List by category
        let external = db.list_emails_with_labels(false, Some("external"), 10).unwrap();
        assert_eq!(external.len(), 1);
        assert_eq!(external[0].email.id, "e2");
    }

    #[test]
    fn test_insert_email_replaces_existing() {
        let db = create_test_db();

        let email1 = make_test_email("same-id", "Original Subject", "original@example.com");
        db.insert_email(&email1).unwrap();

        let email2 = make_test_email("same-id", "Updated Subject", "updated@example.com");
        db.insert_email(&email2).unwrap();

        let retrieved = db.get_email("same-id").unwrap().unwrap();
        assert_eq!(retrieved.subject, Some("Updated Subject".to_string()));
        assert_eq!(retrieved.sender_address, "updated@example.com");

        // Should still be only 1 email
        assert_eq!(db.email_count().unwrap(), 1);
    }
}
