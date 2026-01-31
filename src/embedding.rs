use crate::config::Config;
use crate::db::{Database, Email};
use crate::error::{EmailClError, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

const MAX_TEXT_LENGTH: usize = 8000;

#[derive(Debug, Serialize)]
struct EmbeddingRequest {
    model: String,
    prompt: String,
}

#[derive(Debug, Deserialize)]
struct EmbeddingResponse {
    embedding: Vec<f32>,
}

pub struct EmbeddingService {
    base_url: String,
    model: String,
    http_client: reqwest::Client,
}

impl EmbeddingService {
    pub fn new(config: &Config) -> Self {
        EmbeddingService {
            base_url: config.ollama.base_url.clone(),
            model: config.ollama.embed_model.clone(),
            http_client: reqwest::Client::new(),
        }
    }

    pub async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let url = format!("{}/api/embeddings", self.base_url);

        // Truncate if too long
        let text = if text.len() > MAX_TEXT_LENGTH {
            &text[..MAX_TEXT_LENGTH]
        } else {
            text
        };

        let request = EmbeddingRequest {
            model: self.model.clone(),
            prompt: text.to_string(),
        };

        debug!("Generating embedding for text of length {}", text.len());

        let response = self
            .http_client
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|e| EmailClError::Ollama(format!("Failed to connect to Ollama: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(EmailClError::Ollama(format!(
                "Embedding request failed ({}): {}",
                status, body
            )));
        }

        let result: EmbeddingResponse = response.json().await.map_err(|e| {
            EmailClError::Ollama(format!("Failed to parse embedding response: {}", e))
        })?;

        Ok(result.embedding)
    }

    pub fn format_email_for_embedding(email: &Email) -> String {
        let sender_name = email.sender_name.as_deref().unwrap_or("");
        let subject = email.subject.as_deref().unwrap_or("(no subject)");
        let body = email.body_text.as_deref().unwrap_or("");

        format!(
            "From: {} <{}>\nSubject: {}\n\n{}",
            sender_name, email.sender_address, subject, body
        )
    }

    pub async fn embed_email(&self, email: &Email) -> Result<Vec<f32>> {
        let text = Self::format_email_for_embedding(email);
        self.embed(&text).await
    }

    pub async fn process_unembedded_emails(&self, db: &Database, batch_size: usize) -> Result<usize> {
        let emails = db.get_emails_without_embedding(batch_size)?;

        if emails.is_empty() {
            debug!("No emails to embed");
            return Ok(0);
        }

        info!("Embedding {} emails...", emails.len());
        let mut count = 0;

        for email in &emails {
            match self.embed_email(email).await {
                Ok(embedding) => {
                    db.update_embedding(&email.id, &embedding)?;
                    count += 1;
                    debug!("Embedded email: {}", email.subject.as_deref().unwrap_or("(no subject)"));
                }
                Err(e) => {
                    warn!("Failed to embed email {}: {}", email.id, e);
                }
            }
        }

        info!("Successfully embedded {} emails", count);
        Ok(count)
    }

    pub async fn is_available(&self) -> bool {
        let url = format!("{}/api/tags", self.base_url);

        match self.http_client.get(&url).send().await {
            Ok(response) => response.status().is_success(),
            Err(_) => false,
        }
    }
}

// Vector similarity functions
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }

    let dot_product: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot_product / (norm_a * norm_b)
}

pub fn find_similar_emails<'a>(
    target_embedding: &[f32],
    candidates: &'a [crate::db::EmailWithLabel],
    k: usize,
) -> Vec<(&'a crate::db::EmailWithLabel, f32)> {
    let mut scored: Vec<_> = candidates
        .iter()
        .filter_map(|c| {
            c.email.embedding.as_ref().map(|emb| {
                let similarity = cosine_similarity(target_embedding, emb);
                (c, similarity)
            })
        })
        .collect();

    // Sort by similarity descending
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    scored.into_iter().take(k).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Email, EmailWithLabel, Label};
    use chrono::Utc;

    fn make_test_email(id: &str, subject: &str, sender: &str, embedding: Option<Vec<f32>>) -> Email {
        Email {
            id: id.to_string(),
            subject: Some(subject.to_string()),
            body_text: Some("Test body".to_string()),
            body_html: None,
            sender_address: sender.to_string(),
            sender_name: Some("Test Sender".to_string()),
            received_at: Utc::now(),
            folder_id: None,
            has_attachments: false,
            is_read: false,
            importance: None,
            fetched_at: Utc::now(),
            embedding,
        }
    }

    fn make_test_label(email_id: &str, category: &str, importance: i32) -> Label {
        Label {
            email_id: email_id.to_string(),
            category: category.to_string(),
            importance,
            source: "user".to_string(),
            confidence: Some(0.9),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn test_cosine_similarity_identical_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        let similarity = cosine_similarity(&a, &b);
        assert!((similarity - 1.0).abs() < 0.0001, "Identical vectors should have similarity 1.0");
    }

    #[test]
    fn test_cosine_similarity_orthogonal_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let similarity = cosine_similarity(&a, &b);
        assert!((similarity - 0.0).abs() < 0.0001, "Orthogonal vectors should have similarity 0.0");
    }

    #[test]
    fn test_cosine_similarity_opposite_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![-1.0, 0.0, 0.0];
        let similarity = cosine_similarity(&a, &b);
        assert!((similarity - (-1.0)).abs() < 0.0001, "Opposite vectors should have similarity -1.0");
    }

    #[test]
    fn test_cosine_similarity_different_lengths() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0];
        let similarity = cosine_similarity(&a, &b);
        assert_eq!(similarity, 0.0, "Different length vectors should return 0.0");
    }

    #[test]
    fn test_cosine_similarity_zero_vector() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        let similarity = cosine_similarity(&a, &b);
        assert_eq!(similarity, 0.0, "Zero vector should return 0.0");
    }

    #[test]
    fn test_cosine_similarity_normalized_vectors() {
        // Two vectors at 45 degree angle
        let a = vec![1.0, 0.0];
        let b = vec![0.707107, 0.707107]; // Normalized 45-degree vector
        let similarity = cosine_similarity(&a, &b);
        assert!((similarity - 0.707107).abs() < 0.001, "Expected ~0.707 for 45 degree angle");
    }

    #[test]
    fn test_format_email_for_embedding_complete() {
        let email = make_test_email("1", "Test Subject", "test@example.com", None);
        let formatted = EmbeddingService::format_email_for_embedding(&email);

        assert!(formatted.contains("From: Test Sender <test@example.com>"));
        assert!(formatted.contains("Subject: Test Subject"));
        assert!(formatted.contains("Test body"));
    }

    #[test]
    fn test_format_email_for_embedding_no_sender_name() {
        let mut email = make_test_email("1", "Test Subject", "test@example.com", None);
        email.sender_name = None;
        let formatted = EmbeddingService::format_email_for_embedding(&email);

        assert!(formatted.contains("From:  <test@example.com>"));
    }

    #[test]
    fn test_format_email_for_embedding_no_subject() {
        let mut email = make_test_email("1", "Test Subject", "test@example.com", None);
        email.subject = None;
        let formatted = EmbeddingService::format_email_for_embedding(&email);

        assert!(formatted.contains("Subject: (no subject)"));
    }

    #[test]
    fn test_format_email_for_embedding_no_body() {
        let mut email = make_test_email("1", "Test Subject", "test@example.com", None);
        email.body_text = None;
        let formatted = EmbeddingService::format_email_for_embedding(&email);

        // Should still contain the header parts
        assert!(formatted.contains("From:"));
        assert!(formatted.contains("Subject:"));
    }

    #[test]
    fn test_find_similar_emails_returns_top_k() {
        let target = vec![1.0, 0.0, 0.0];

        let candidates = vec![
            EmailWithLabel {
                email: make_test_email("1", "Subject 1", "a@example.com", Some(vec![1.0, 0.0, 0.0])),
                label: Some(make_test_label("1", "internal", 2)),
            },
            EmailWithLabel {
                email: make_test_email("2", "Subject 2", "b@example.com", Some(vec![0.9, 0.1, 0.0])),
                label: Some(make_test_label("2", "external", 1)),
            },
            EmailWithLabel {
                email: make_test_email("3", "Subject 3", "c@example.com", Some(vec![0.0, 1.0, 0.0])),
                label: Some(make_test_label("3", "junk", 0)),
            },
        ];

        let results = find_similar_emails(&target, &candidates, 2);

        assert_eq!(results.len(), 2, "Should return exactly k results");
        assert_eq!(results[0].0.email.id, "1", "Most similar should be first");
        assert_eq!(results[1].0.email.id, "2", "Second most similar should be second");
    }

    #[test]
    fn test_find_similar_emails_skips_no_embedding() {
        let target = vec![1.0, 0.0, 0.0];

        let candidates = vec![
            EmailWithLabel {
                email: make_test_email("1", "Subject 1", "a@example.com", Some(vec![1.0, 0.0, 0.0])),
                label: Some(make_test_label("1", "internal", 2)),
            },
            EmailWithLabel {
                email: make_test_email("2", "Subject 2", "b@example.com", None), // No embedding
                label: Some(make_test_label("2", "external", 1)),
            },
        ];

        let results = find_similar_emails(&target, &candidates, 5);

        assert_eq!(results.len(), 1, "Should skip emails without embeddings");
        assert_eq!(results[0].0.email.id, "1");
    }

    #[test]
    fn test_find_similar_emails_empty_candidates() {
        let target = vec![1.0, 0.0, 0.0];
        let candidates: Vec<EmailWithLabel> = vec![];

        let results = find_similar_emails(&target, &candidates, 5);

        assert!(results.is_empty(), "Empty candidates should return empty results");
    }

    #[test]
    fn test_find_similar_emails_k_larger_than_candidates() {
        let target = vec![1.0, 0.0, 0.0];

        let candidates = vec![
            EmailWithLabel {
                email: make_test_email("1", "Subject 1", "a@example.com", Some(vec![1.0, 0.0, 0.0])),
                label: Some(make_test_label("1", "internal", 2)),
            },
        ];

        let results = find_similar_emails(&target, &candidates, 10);

        assert_eq!(results.len(), 1, "Should return all available when k > candidates");
    }
}
