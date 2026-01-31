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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ClassifierConfig, Config, OllamaConfig};
    use crate::db::{Email, EmailWithLabel, Label};
    use chrono::Utc;

    fn make_test_config() -> Config {
        Config {
            ollama: OllamaConfig {
                base_url: "http://localhost:11434".to_string(),
                embed_model: "nomic-embed-text".to_string(),
                classify_model: "qwen2.5:7b".to_string(),
            },
            classifier: ClassifierConfig {
                similar_count: 5,
                min_confidence: 0.7,
            },
            ..Config::default()
        }
    }

    fn make_test_email(id: &str, subject: &str, sender: &str, body: &str) -> Email {
        Email {
            id: id.to_string(),
            subject: Some(subject.to_string()),
            body_text: Some(body.to_string()),
            body_html: None,
            sender_address: sender.to_string(),
            sender_name: Some("Test Sender".to_string()),
            received_at: Utc::now(),
            folder_id: None,
            has_attachments: false,
            is_read: false,
            importance: None,
            fetched_at: Utc::now(),
            embedding: None,
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
    fn test_classifier_creation() {
        let config = make_test_config();
        let classifier = Classifier::new(&config);

        assert_eq!(classifier.base_url, "http://localhost:11434");
        assert_eq!(classifier.model, "qwen2.5:7b");
        assert_eq!(classifier.similar_count, 5);
        assert!((classifier.min_confidence - 0.7).abs() < 0.001);
    }

    #[test]
    fn test_build_prompt_no_examples() {
        let config = make_test_config();
        let classifier = Classifier::new(&config);
        let email = make_test_email("e1", "Test Subject", "test@example.com", "Test body content");

        let examples: Vec<(&EmailWithLabel, f32)> = vec![];
        let prompt = classifier.build_prompt(&email, &examples);

        // Should contain system prompt
        assert!(prompt.contains("You are an email classifier"));
        assert!(prompt.contains("Categories:"));
        assert!(prompt.contains("Importance levels:"));

        // Should contain email details
        assert!(prompt.contains("Test Sender <test@example.com>"));
        assert!(prompt.contains("Subject: Test Subject"));
        assert!(prompt.contains("Test body content"));

        // Should NOT contain examples section header
        assert!(!prompt.contains("Examples of previously classified emails:"));

        // Should contain response format
        assert!(prompt.contains("Respond with JSON only:"));
    }

    #[test]
    fn test_build_prompt_with_examples() {
        let config = make_test_config();
        let classifier = Classifier::new(&config);
        let email = make_test_email("e1", "New Email", "new@example.com", "New body");

        let example_email = make_test_email("ex1", "Example Subject", "example@company.com", "Example body");
        let example_label = make_test_label("ex1", "internal", 2);
        let example = EmailWithLabel {
            email: example_email,
            label: Some(example_label),
        };

        let examples: Vec<(&EmailWithLabel, f32)> = vec![(&example, 0.85)];
        let prompt = classifier.build_prompt(&email, &examples);

        // Should contain examples section
        assert!(prompt.contains("Examples of previously classified emails:"));

        // Should contain example details
        assert!(prompt.contains("example@company.com"));
        assert!(prompt.contains("Example Subject"));
        assert!(prompt.contains("Category: internal"));
        assert!(prompt.contains("Importance: 2"));
        assert!(prompt.contains("similarity: 0.85"));
    }

    #[test]
    fn test_build_prompt_truncates_long_body() {
        let config = make_test_config();
        let classifier = Classifier::new(&config);

        // Create a very long body (> 2000 chars)
        let long_body = "A".repeat(3000);
        let email = make_test_email("e1", "Long Body Email", "long@example.com", &long_body);

        let examples: Vec<(&EmailWithLabel, f32)> = vec![];
        let prompt = classifier.build_prompt(&email, &examples);

        // Should contain truncated body with "..."
        assert!(prompt.contains("..."));
        // Should not contain the full body
        assert!(!prompt.contains(&"A".repeat(3000)));
    }

    #[test]
    fn test_build_prompt_handles_missing_subject() {
        let config = make_test_config();
        let classifier = Classifier::new(&config);

        let mut email = make_test_email("e1", "Placeholder", "test@example.com", "Body");
        email.subject = None;

        let examples: Vec<(&EmailWithLabel, f32)> = vec![];
        let prompt = classifier.build_prompt(&email, &examples);

        assert!(prompt.contains("Subject: (no subject)"));
    }

    #[test]
    fn test_build_prompt_handles_missing_sender_name() {
        let config = make_test_config();
        let classifier = Classifier::new(&config);

        let mut email = make_test_email("e1", "Subject", "test@example.com", "Body");
        email.sender_name = None;

        let examples: Vec<(&EmailWithLabel, f32)> = vec![];
        let prompt = classifier.build_prompt(&email, &examples);

        assert!(prompt.contains("From:  <test@example.com>"));
    }

    #[test]
    fn test_classification_result_parsing() {
        // Test parsing of valid JSON response
        let json = r#"{"category": "external", "importance": 3, "reason": "Email from outside organization"}"#;
        let result: ClassificationResult = serde_json::from_str(json).expect("Failed to parse");

        assert_eq!(result.category, "external");
        assert_eq!(result.importance, 3);
        assert_eq!(result.reason, Some("Email from outside organization".to_string()));
    }

    #[test]
    fn test_classification_result_parsing_without_reason() {
        let json = r#"{"category": "junk", "importance": 0}"#;
        let result: ClassificationResult = serde_json::from_str(json).expect("Failed to parse");

        assert_eq!(result.category, "junk");
        assert_eq!(result.importance, 0);
        assert!(result.reason.is_none());
    }

    #[test]
    fn test_classification_result_parsing_all_categories() {
        let categories = ["hr", "system_alert", "system_ok", "external", "internal", "junk"];

        for cat in categories {
            let json = format!(r#"{{"category": "{}", "importance": 2}}"#, cat);
            let result: ClassificationResult = serde_json::from_str(&json).expect("Failed to parse");
            assert_eq!(result.category, cat);
        }
    }

    #[test]
    fn test_classification_result_importance_range() {
        // Test various importance values
        for importance in 0..=4 {
            let json = format!(r#"{{"category": "internal", "importance": {}}}"#, importance);
            let result: ClassificationResult = serde_json::from_str(&json).expect("Failed to parse");
            assert_eq!(result.importance, importance);
        }
    }

    #[test]
    fn test_system_prompt_contains_all_categories() {
        assert!(SYSTEM_PROMPT.contains("hr:"));
        assert!(SYSTEM_PROMPT.contains("system_alert:"));
        assert!(SYSTEM_PROMPT.contains("system_ok:"));
        assert!(SYSTEM_PROMPT.contains("external:"));
        assert!(SYSTEM_PROMPT.contains("internal:"));
        assert!(SYSTEM_PROMPT.contains("junk:"));
    }

    #[test]
    fn test_system_prompt_contains_importance_levels() {
        assert!(SYSTEM_PROMPT.contains("- 0:"));
        assert!(SYSTEM_PROMPT.contains("- 1:"));
        assert!(SYSTEM_PROMPT.contains("- 2:"));
        assert!(SYSTEM_PROMPT.contains("- 3:"));
        assert!(SYSTEM_PROMPT.contains("- 4:"));
    }

    #[test]
    fn test_build_prompt_multiple_examples() {
        let config = make_test_config();
        let classifier = Classifier::new(&config);
        let email = make_test_email("new", "New Email", "new@example.com", "New body");

        let example1 = EmailWithLabel {
            email: make_test_email("ex1", "HR Update", "hr@company.com", "Policy update"),
            label: Some(make_test_label("ex1", "hr", 2)),
        };

        let example2 = EmailWithLabel {
            email: make_test_email("ex2", "Alert: Server Down", "alert@monitoring.com", "Server is down"),
            label: Some(make_test_label("ex2", "system_alert", 4)),
        };

        let examples: Vec<(&EmailWithLabel, f32)> = vec![(&example1, 0.9), (&example2, 0.75)];
        let prompt = classifier.build_prompt(&email, &examples);

        // Should contain both examples
        assert!(prompt.contains("hr@company.com"));
        assert!(prompt.contains("HR Update"));
        assert!(prompt.contains("Category: hr"));

        assert!(prompt.contains("alert@monitoring.com"));
        assert!(prompt.contains("Alert: Server Down"));
        assert!(prompt.contains("Category: system_alert"));
    }
}
