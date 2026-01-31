use crate::config::Config;
use crate::db::{Database, Email, EmailWithLabel, Label};
use crate::embedding::{find_similar_emails, EmbeddingService};
use crate::error::{EmailClError, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

const SYSTEM_PROMPT: &str = r#"You are an email classifier. Based on the examples below, classify the new email.

Categories:
- hr: Human resources communications, policies, benefits
- system_alert: Automated system notifications, monitoring alerts, CI/CD failures
- system_ok: Routine system status, successful deployments, all-clear notices
- external: Emails from outside the organisation
- internal: General internal communications
- junk: Newsletters, marketing, low-priority automated emails

Importance levels:
- 0: Can ignore entirely
- 1: Low - review when convenient
- 2: Normal - standard priority
- 3: High - needs attention soon
- 4: Urgent - immediate attention required

"#;

#[derive(Debug, Serialize)]
struct GenerateRequest {
    model: String,
    prompt: String,
    stream: bool,
    format: String,
}

#[derive(Debug, Deserialize)]
struct GenerateResponse {
    response: String,
}

#[derive(Debug, Deserialize)]
pub struct ClassificationResult {
    pub category: String,
    pub importance: i32,
    pub reason: Option<String>,
}

pub struct Classifier {
    base_url: String,
    model: String,
    similar_count: usize,
    min_confidence: f64,
    http_client: reqwest::Client,
}

impl Classifier {
    pub fn new(config: &Config) -> Self {
        Classifier {
            base_url: config.ollama.base_url.clone(),
            model: config.ollama.classify_model.clone(),
            similar_count: config.classifier.similar_count,
            min_confidence: config.classifier.min_confidence,
            http_client: reqwest::Client::new(),
        }
    }

    pub async fn classify(
        &self,
        email: &Email,
        examples: &[(&EmailWithLabel, f32)],
    ) -> Result<ClassificationResult> {
        let prompt = self.build_prompt(email, examples);

        debug!("Classifying email: {}", email.subject.as_deref().unwrap_or("(no subject)"));

        let request = GenerateRequest {
            model: self.model.clone(),
            prompt,
            stream: false,
            format: "json".to_string(),
        };

        let url = format!("{}/api/generate", self.base_url);

        let response = self
            .http_client
            .post(&url)
            .json(&request)
            .timeout(std::time::Duration::from_secs(120))
            .send()
            .await
            .map_err(|e| EmailClError::Ollama(format!("Failed to connect to Ollama: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(EmailClError::Ollama(format!(
                "Classification request failed ({}): {}",
                status, body
            )));
        }

        let result: GenerateResponse = response.json().await.map_err(|e| {
            EmailClError::Ollama(format!("Failed to parse classification response: {}", e))
        })?;

        // Parse JSON response
        let classification: ClassificationResult =
            serde_json::from_str(&result.response).map_err(|e| {
                EmailClError::Classification(format!(
                    "Failed to parse classification JSON: {}. Response was: {}",
                    e, result.response
                ))
            })?;

        // Validate category
        let valid_categories = ["hr", "system_alert", "system_ok", "external", "internal", "junk"];
        if !valid_categories.contains(&classification.category.as_str()) {
            warn!(
                "Unknown category '{}', defaulting to 'internal'",
                classification.category
            );
        }

        // Validate importance
        let importance = classification.importance.clamp(0, 4);

        Ok(ClassificationResult {
            category: classification.category,
            importance,
            reason: classification.reason,
        })
    }

    fn build_prompt(&self, email: &Email, examples: &[(&EmailWithLabel, f32)]) -> String {
        let mut prompt = String::from(SYSTEM_PROMPT);

        if !examples.is_empty() {
            prompt.push_str("Examples of previously classified emails:\n\n");

            for (example, similarity) in examples {
                let label = example.label.as_ref().unwrap();
                prompt.push_str("---\n");
                prompt.push_str(&format!(
                    "From: {} <{}>\n",
                    example.email.sender_name.as_deref().unwrap_or(""),
                    example.email.sender_address
                ));
                prompt.push_str(&format!(
                    "Subject: {}\n",
                    example.email.subject.as_deref().unwrap_or("(no subject)")
                ));
                prompt.push_str(&format!("Category: {}\n", label.category));
                prompt.push_str(&format!("Importance: {}\n", label.importance));
                prompt.push_str(&format!("(similarity: {:.2})\n", similarity));
                prompt.push_str("---\n\n");
            }
        }

        prompt.push_str("Now classify this email:\n\n");
        prompt.push_str(&format!(
            "From: {} <{}>\n",
            email.sender_name.as_deref().unwrap_or(""),
            email.sender_address
        ));
        prompt.push_str(&format!(
            "Subject: {}\n",
            email.subject.as_deref().unwrap_or("(no subject)")
        ));
        prompt.push_str(&format!("Received: {}\n\n", email.received_at.format("%Y-%m-%d %H:%M")));

        // Add truncated body
        let body = email.body_text.as_deref().unwrap_or("");
        let truncated_body = if body.len() > 2000 {
            format!("{}...", &body[..2000])
        } else {
            body.to_string()
        };
        prompt.push_str(&truncated_body);

        prompt.push_str("\n\nRespond with JSON only:\n{\"category\": \"...\", \"importance\": N, \"reason\": \"brief explanation\"}");

        prompt
    }

    pub async fn classify_and_store(
        &self,
        db: &Database,
        email: &Email,
        _embedding_service: &EmbeddingService,
    ) -> Result<Label> {
        // Get similar verified emails for few-shot learning
        let verified = db.get_verified_emails_with_embeddings(100)?;

        let examples = if let Some(ref embedding) = email.embedding {
            find_similar_emails(embedding, &verified, self.similar_count)
        } else {
            vec![]
        };

        let result = self.classify(email, &examples).await?;

        // Calculate confidence based on average similarity of examples
        let confidence = if examples.is_empty() {
            0.5 // Low confidence when no examples
        } else {
            let avg_similarity: f32 = examples.iter().map(|(_, s)| s).sum::<f32>() / examples.len() as f32;
            avg_similarity as f64
        };

        let label = Label {
            email_id: email.id.clone(),
            category: result.category,
            importance: result.importance,
            source: "model".to_string(),
            confidence: Some(confidence),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        db.insert_label(&label)?;

        if let Some(reason) = result.reason {
            debug!("Classification reason: {}", reason);
        }

        Ok(label)
    }

    pub async fn process_unclassified_emails(
        &self,
        db: &Database,
        embedding_service: &EmbeddingService,
        batch_size: usize,
    ) -> Result<usize> {
        let emails = db.get_emails_without_label(batch_size)?;

        if emails.is_empty() {
            debug!("No emails to classify");
            return Ok(0);
        }

        info!("Classifying {} emails...", emails.len());
        let mut count = 0;

        for email in &emails {
            match self.classify_and_store(db, email, embedding_service).await {
                Ok(label) => {
                    count += 1;
                    info!(
                        "Classified '{}' as {} (importance: {})",
                        email.subject.as_deref().unwrap_or("(no subject)"),
                        label.category,
                        label.importance
                    );
                }
                Err(e) => {
                    warn!("Failed to classify email {}: {}", email.id, e);
                }
            }
        }

        info!("Successfully classified {} emails", count);
        Ok(count)
    }
}
