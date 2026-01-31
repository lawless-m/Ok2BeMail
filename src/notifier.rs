use crate::config::{NotifyConfig, NotifyRule};
use crate::db::{Email, Label};
use crate::error::{EmailClError, Result};
use notify_rust::Notification;
use tracing::{debug, info, warn};

pub struct Notifier {
    enabled: bool,
    rules: Vec<NotifyRule>,
}

impl Notifier {
    pub fn new(config: &NotifyConfig) -> Self {
        Notifier {
            enabled: config.enabled,
            rules: config.rules.clone(),
        }
    }

    pub fn should_notify(&self, label: &Label) -> bool {
        if !self.enabled {
            return false;
        }

        for rule in &self.rules {
            if rule.category == label.category && label.importance >= rule.min_importance {
                return true;
            }
        }

        false
    }

    pub fn notify(&self, email: &Email, label: &Label) -> Result<()> {
        if !self.should_notify(label) {
            debug!(
                "Skipping notification for email '{}' (category: {}, importance: {})",
                email.subject.as_deref().unwrap_or("(no subject)"),
                label.category,
                label.importance
            );
            return Ok(());
        }

        let subject = email.subject.as_deref().unwrap_or("(no subject)");
        let sender = email
            .sender_name
            .as_deref()
            .unwrap_or(&email.sender_address);

        let title = format!("[{}] {}", capitalize(&label.category), truncate(subject, 50));
        let body = format!("From: {}", sender);

        info!(
            "Sending notification for email '{}' ({}, importance: {})",
            subject, label.category, label.importance
        );

        self.send_desktop_notification(&title, &body)?;

        Ok(())
    }

    fn send_desktop_notification(&self, title: &str, body: &str) -> Result<()> {
        Notification::new()
            .summary(title)
            .body(body)
            .appname("emailcl")
            .timeout(notify_rust::Timeout::Milliseconds(10000))
            .show()
            .map_err(|e| EmailClError::Notification(format!("Failed to show notification: {}", e)))?;

        Ok(())
    }

    pub fn notify_batch(&self, emails_and_labels: &[(Email, Label)]) -> Result<usize> {
        let mut count = 0;

        for (email, label) in emails_and_labels {
            if self.should_notify(label) {
                match self.notify(email, label) {
                    Ok(_) => count += 1,
                    Err(e) => warn!("Notification failed: {}", e),
                }
            }
        }

        if count > 0 {
            info!("Sent {} notifications", count);
        }

        Ok(count)
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().chain(chars).collect(),
    }
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len - 3])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capitalize() {
        assert_eq!(capitalize("hello"), "Hello");
        assert_eq!(capitalize("HELLO"), "HELLO");
        assert_eq!(capitalize(""), "");
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("this is a long string", 10), "this is...");
    }
}
