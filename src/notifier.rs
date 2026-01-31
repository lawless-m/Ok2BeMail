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
    use chrono::Utc;

    fn make_test_email(id: &str, subject: &str, sender_address: &str, sender_name: Option<&str>) -> Email {
        Email {
            id: id.to_string(),
            subject: Some(subject.to_string()),
            body_text: Some("Test body".to_string()),
            body_html: None,
            sender_address: sender_address.to_string(),
            sender_name: sender_name.map(|s| s.to_string()),
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
            source: "model".to_string(),
            confidence: Some(0.9),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn make_test_notify_config(enabled: bool, rules: Vec<NotifyRule>) -> NotifyConfig {
        NotifyConfig { enabled, rules }
    }

    #[test]
    fn test_capitalize() {
        assert_eq!(capitalize("hello"), "Hello");
        assert_eq!(capitalize("HELLO"), "HELLO");
        assert_eq!(capitalize(""), "");
    }

    #[test]
    fn test_capitalize_unicode() {
        assert_eq!(capitalize("über"), "Über");
        assert_eq!(capitalize("ñoño"), "Ñoño");
    }

    #[test]
    fn test_capitalize_single_char() {
        assert_eq!(capitalize("a"), "A");
        assert_eq!(capitalize("Z"), "Z");
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("this is a long string", 10), "this is...");
    }

    #[test]
    fn test_truncate_exact_length() {
        assert_eq!(truncate("exactly10!", 10), "exactly10!");
    }

    #[test]
    fn test_truncate_one_over() {
        assert_eq!(truncate("exactly11!!", 10), "exactly...");
    }

    #[test]
    fn test_truncate_empty() {
        assert_eq!(truncate("", 10), "");
    }

    #[test]
    fn test_notifier_creation() {
        let rules = vec![
            NotifyRule {
                category: "system_alert".to_string(),
                min_importance: 3,
                action: "desktop".to_string(),
            },
        ];
        let config = make_test_notify_config(true, rules);
        let notifier = Notifier::new(&config);

        assert!(notifier.enabled);
        assert_eq!(notifier.rules.len(), 1);
    }

    #[test]
    fn test_should_notify_disabled() {
        let config = make_test_notify_config(false, vec![
            NotifyRule {
                category: "system_alert".to_string(),
                min_importance: 3,
                action: "desktop".to_string(),
            },
        ]);
        let notifier = Notifier::new(&config);
        let label = make_test_label("e1", "system_alert", 4);

        // Even with matching rule, disabled notifier should not notify
        assert!(!notifier.should_notify(&label));
    }

    #[test]
    fn test_should_notify_matching_rule() {
        let config = make_test_notify_config(true, vec![
            NotifyRule {
                category: "system_alert".to_string(),
                min_importance: 3,
                action: "desktop".to_string(),
            },
        ]);
        let notifier = Notifier::new(&config);

        // High importance system alert should trigger
        let label = make_test_label("e1", "system_alert", 4);
        assert!(notifier.should_notify(&label));

        // Exactly at threshold should trigger
        let label = make_test_label("e2", "system_alert", 3);
        assert!(notifier.should_notify(&label));
    }

    #[test]
    fn test_should_notify_below_threshold() {
        let config = make_test_notify_config(true, vec![
            NotifyRule {
                category: "system_alert".to_string(),
                min_importance: 3,
                action: "desktop".to_string(),
            },
        ]);
        let notifier = Notifier::new(&config);

        // Below threshold should not trigger
        let label = make_test_label("e1", "system_alert", 2);
        assert!(!notifier.should_notify(&label));

        let label = make_test_label("e2", "system_alert", 0);
        assert!(!notifier.should_notify(&label));
    }

    #[test]
    fn test_should_notify_wrong_category() {
        let config = make_test_notify_config(true, vec![
            NotifyRule {
                category: "system_alert".to_string(),
                min_importance: 3,
                action: "desktop".to_string(),
            },
        ]);
        let notifier = Notifier::new(&config);

        // High importance but wrong category
        let label = make_test_label("e1", "junk", 4);
        assert!(!notifier.should_notify(&label));

        let label = make_test_label("e2", "external", 4);
        assert!(!notifier.should_notify(&label));
    }

    #[test]
    fn test_should_notify_multiple_rules() {
        let config = make_test_notify_config(true, vec![
            NotifyRule {
                category: "system_alert".to_string(),
                min_importance: 3,
                action: "desktop".to_string(),
            },
            NotifyRule {
                category: "external".to_string(),
                min_importance: 2,
                action: "desktop".to_string(),
            },
            NotifyRule {
                category: "hr".to_string(),
                min_importance: 1,
                action: "desktop".to_string(),
            },
        ]);
        let notifier = Notifier::new(&config);

        // Should match system_alert rule
        assert!(notifier.should_notify(&make_test_label("e1", "system_alert", 3)));

        // Should match external rule
        assert!(notifier.should_notify(&make_test_label("e2", "external", 2)));

        // Should match hr rule
        assert!(notifier.should_notify(&make_test_label("e3", "hr", 1)));

        // Should not match - junk not in rules
        assert!(!notifier.should_notify(&make_test_label("e4", "junk", 4)));
    }

    #[test]
    fn test_should_notify_empty_rules() {
        let config = make_test_notify_config(true, vec![]);
        let notifier = Notifier::new(&config);

        // No rules means nothing triggers
        let label = make_test_label("e1", "system_alert", 4);
        assert!(!notifier.should_notify(&label));
    }

    #[test]
    fn test_should_notify_importance_zero() {
        let config = make_test_notify_config(true, vec![
            NotifyRule {
                category: "junk".to_string(),
                min_importance: 0,
                action: "desktop".to_string(),
            },
        ]);
        let notifier = Notifier::new(&config);

        // With min_importance 0, any junk should trigger
        let label = make_test_label("e1", "junk", 0);
        assert!(notifier.should_notify(&label));
    }

    #[test]
    fn test_notifier_rules_cloned_from_config() {
        let rules = vec![
            NotifyRule {
                category: "test".to_string(),
                min_importance: 2,
                action: "desktop".to_string(),
            },
        ];
        let config = make_test_notify_config(true, rules.clone());
        let notifier = Notifier::new(&config);

        // Rules should be cloned, not moved
        assert_eq!(notifier.rules.len(), 1);
        assert_eq!(notifier.rules[0].category, "test");
        assert_eq!(notifier.rules[0].min_importance, 2);
    }

    #[test]
    fn test_notify_rule_action_field() {
        let rule = NotifyRule {
            category: "system_alert".to_string(),
            min_importance: 3,
            action: "email".to_string(),
        };

        // Action field is preserved even if not currently used
        assert_eq!(rule.action, "email");
    }

    #[test]
    fn test_truncate_subject_for_notification() {
        // Test that long subjects get truncated properly for notifications
        let long_subject = "A".repeat(100);
        let truncated = truncate(&long_subject, 50);

        assert!(truncated.len() <= 50);
        assert!(truncated.ends_with("..."));
    }
}
