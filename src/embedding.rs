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
