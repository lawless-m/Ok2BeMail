use crate::error::{EmailClError, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub azure: AzureConfig,
    pub sync: SyncConfig,
    pub ollama: OllamaConfig,
    pub classifier: ClassifierConfig,
    pub storage: StorageConfig,
    pub notify: NotifyConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AzureConfig {
    pub client_id: String,
    pub tenant_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    #[serde(default = "default_initial_days")]
    pub initial_days: u32,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
    #[serde(default = "default_folders")]
    pub folders: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaConfig {
    #[serde(default = "default_ollama_url")]
    pub base_url: String,
    #[serde(default = "default_embed_model")]
    pub embed_model: String,
    #[serde(default = "default_classify_model")]
    pub classify_model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassifierConfig {
    #[serde(default = "default_similar_count")]
    pub similar_count: usize,
    #[serde(default = "default_min_confidence")]
    pub min_confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    #[serde(default = "default_db_path")]
    pub db_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotifyConfig {
    #[serde(default = "default_notify_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub rules: Vec<NotifyRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotifyRule {
    pub category: String,
    pub min_importance: i32,
    #[serde(default = "default_action")]
    pub action: String,
}

// Default values
fn default_initial_days() -> u32 { 30 }
fn default_poll_interval() -> u64 { 300 }
fn default_folders() -> Vec<String> { vec!["inbox".to_string()] }
fn default_ollama_url() -> String { "http://localhost:11434".to_string() }
fn default_embed_model() -> String { "nomic-embed-text".to_string() }
fn default_classify_model() -> String { "qwen2.5:7b".to_string() }
fn default_similar_count() -> usize { 8 }
fn default_min_confidence() -> f64 { 0.7 }
fn default_notify_enabled() -> bool { true }
fn default_action() -> String { "desktop".to_string() }

fn default_db_path() -> String {
    get_data_dir()
        .join("emails.db")
        .to_string_lossy()
        .to_string()
}

impl Default for Config {
    fn default() -> Self {
        Config {
            azure: AzureConfig {
                client_id: String::new(),
                tenant_id: String::new(),
            },
            sync: SyncConfig {
                initial_days: default_initial_days(),
                poll_interval_secs: default_poll_interval(),
                folders: default_folders(),
            },
            ollama: OllamaConfig {
                base_url: default_ollama_url(),
                embed_model: default_embed_model(),
                classify_model: default_classify_model(),
            },
            classifier: ClassifierConfig {
                similar_count: default_similar_count(),
                min_confidence: default_min_confidence(),
            },
            storage: StorageConfig {
                db_path: default_db_path(),
            },
            notify: NotifyConfig {
                enabled: default_notify_enabled(),
                rules: default_notify_rules(),
            },
        }
    }
}

fn default_notify_rules() -> Vec<NotifyRule> {
    vec![
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
            min_importance: 3,
            action: "desktop".to_string(),
        },
    ]
}

pub fn get_config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("emailcl")
}

pub fn get_data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("emailcl")
}

pub fn get_config_path() -> PathBuf {
    get_config_dir().join("config.toml")
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_path = get_config_path();

        if !config_path.exists() {
            return Err(EmailClError::Config(format!(
                "Config file not found at {}. Run 'emailcl init' first.",
                config_path.display()
            )));
        }

        let content = std::fs::read_to_string(&config_path)?;
        let config: Config = toml::from_str(&content)
            .map_err(|e| EmailClError::Config(format!("Failed to parse config: {}", e)))?;

        config.validate()?;
        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        let config_dir = get_config_dir();
        std::fs::create_dir_all(&config_dir)?;

        let config_path = get_config_path();
        let content = toml::to_string_pretty(self)
            .map_err(|e| EmailClError::Config(format!("Failed to serialize config: {}", e)))?;

        std::fs::write(&config_path, content)?;

        // Set restrictive permissions on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(&config_path, perms)?;
        }

        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if self.azure.client_id.is_empty() {
            return Err(EmailClError::Config("Azure client_id is required".to_string()));
        }
        if self.azure.tenant_id.is_empty() {
            return Err(EmailClError::Config("Azure tenant_id is required".to_string()));
        }
        Ok(())
    }

    pub fn expanded_db_path(&self) -> PathBuf {
        let path = &self.storage.db_path;
        if path.starts_with("~/") {
            if let Some(home) = dirs::home_dir() {
                return home.join(&path[2..]);
            }
        }
        PathBuf::from(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_valid_config() -> Config {
        Config {
            azure: AzureConfig {
                client_id: "test-client-id-12345".to_string(),
                tenant_id: "test-tenant-id-67890".to_string(),
            },
            sync: SyncConfig {
                initial_days: 14,
                poll_interval_secs: 180,
                folders: vec!["inbox".to_string(), "sent".to_string()],
            },
            ollama: OllamaConfig {
                base_url: "http://localhost:11434".to_string(),
                embed_model: "nomic-embed-text".to_string(),
                classify_model: "qwen2.5:7b".to_string(),
            },
            classifier: ClassifierConfig {
                similar_count: 5,
                min_confidence: 0.8,
            },
            storage: StorageConfig {
                db_path: "/tmp/test-emails.db".to_string(),
            },
            notify: NotifyConfig {
                enabled: true,
                rules: vec![
                    NotifyRule {
                        category: "system_alert".to_string(),
                        min_importance: 3,
                        action: "desktop".to_string(),
                    },
                ],
            },
        }
    }

    #[test]
    fn test_default_config() {
        let config = Config::default();

        assert_eq!(config.azure.client_id, "");
        assert_eq!(config.azure.tenant_id, "");
        assert_eq!(config.sync.initial_days, 30);
        assert_eq!(config.sync.poll_interval_secs, 300);
        assert_eq!(config.sync.folders, vec!["inbox".to_string()]);
        assert_eq!(config.ollama.base_url, "http://localhost:11434");
        assert_eq!(config.ollama.embed_model, "nomic-embed-text");
        assert_eq!(config.ollama.classify_model, "qwen2.5:7b");
        assert_eq!(config.classifier.similar_count, 8);
        assert!((config.classifier.min_confidence - 0.7).abs() < 0.001);
        assert!(config.notify.enabled);
    }

    #[test]
    fn test_default_notify_rules() {
        let config = Config::default();

        assert!(!config.notify.rules.is_empty());

        // Should have rules for system_alert, external, and hr
        let categories: Vec<_> = config.notify.rules.iter().map(|r| r.category.as_str()).collect();
        assert!(categories.contains(&"system_alert"));
        assert!(categories.contains(&"external"));
        assert!(categories.contains(&"hr"));
    }

    #[test]
    fn test_validate_valid_config() {
        let config = create_valid_config();
        let result = config.validate();
        assert!(result.is_ok(), "Valid config should pass validation");
    }

    #[test]
    fn test_validate_missing_client_id() {
        let mut config = create_valid_config();
        config.azure.client_id = "".to_string();

        let result = config.validate();
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(err.to_string().contains("client_id"));
    }

    #[test]
    fn test_validate_missing_tenant_id() {
        let mut config = create_valid_config();
        config.azure.tenant_id = "".to_string();

        let result = config.validate();
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(err.to_string().contains("tenant_id"));
    }

    #[test]
    fn test_config_serialization_roundtrip() {
        let config = create_valid_config();

        // Serialize to TOML
        let toml_str = toml::to_string_pretty(&config).expect("Failed to serialize config");

        // Deserialize back
        let deserialized: Config = toml::from_str(&toml_str).expect("Failed to deserialize config");

        // Verify key fields
        assert_eq!(deserialized.azure.client_id, config.azure.client_id);
        assert_eq!(deserialized.azure.tenant_id, config.azure.tenant_id);
        assert_eq!(deserialized.sync.initial_days, config.sync.initial_days);
        assert_eq!(deserialized.sync.poll_interval_secs, config.sync.poll_interval_secs);
        assert_eq!(deserialized.ollama.base_url, config.ollama.base_url);
        assert_eq!(deserialized.classifier.similar_count, config.classifier.similar_count);
        assert_eq!(deserialized.storage.db_path, config.storage.db_path);
        assert_eq!(deserialized.notify.enabled, config.notify.enabled);
        assert_eq!(deserialized.notify.rules.len(), config.notify.rules.len());
    }

    #[test]
    fn test_config_deserialization_with_defaults() {
        // Minimal TOML with only required fields
        let toml_str = r#"
[azure]
client_id = "my-client-id"
tenant_id = "my-tenant-id"

[sync]

[ollama]

[classifier]

[storage]

[notify]
"#;

        let config: Config = toml::from_str(toml_str).expect("Failed to parse minimal config");

        assert_eq!(config.azure.client_id, "my-client-id");
        assert_eq!(config.azure.tenant_id, "my-tenant-id");
        // Should use defaults for other fields
        assert_eq!(config.sync.initial_days, 30);
        assert_eq!(config.sync.poll_interval_secs, 300);
        assert_eq!(config.ollama.base_url, "http://localhost:11434");
        assert_eq!(config.classifier.similar_count, 8);
    }

    #[test]
    fn test_expanded_db_path_absolute() {
        let mut config = create_valid_config();
        config.storage.db_path = "/absolute/path/to/db.sqlite".to_string();

        let expanded = config.expanded_db_path();
        assert_eq!(expanded, PathBuf::from("/absolute/path/to/db.sqlite"));
    }

    #[test]
    fn test_expanded_db_path_tilde() {
        let mut config = create_valid_config();
        config.storage.db_path = "~/mydata/emails.db".to_string();

        let expanded = config.expanded_db_path();

        // Should expand ~ to home directory
        if let Some(home) = dirs::home_dir() {
            assert_eq!(expanded, home.join("mydata/emails.db"));
        }
    }

    #[test]
    fn test_expanded_db_path_relative() {
        let mut config = create_valid_config();
        config.storage.db_path = "data/emails.db".to_string();

        let expanded = config.expanded_db_path();
        assert_eq!(expanded, PathBuf::from("data/emails.db"));
    }

    #[test]
    fn test_notify_rule_serialization() {
        let rule = NotifyRule {
            category: "urgent".to_string(),
            min_importance: 4,
            action: "desktop".to_string(),
        };

        let toml_str = toml::to_string(&rule).expect("Failed to serialize rule");
        assert!(toml_str.contains("category = \"urgent\""));
        assert!(toml_str.contains("min_importance = 4"));
        assert!(toml_str.contains("action = \"desktop\""));
    }

    #[test]
    fn test_sync_config_defaults() {
        let sync = SyncConfig {
            initial_days: default_initial_days(),
            poll_interval_secs: default_poll_interval(),
            folders: default_folders(),
        };

        assert_eq!(sync.initial_days, 30);
        assert_eq!(sync.poll_interval_secs, 300);
        assert_eq!(sync.folders, vec!["inbox".to_string()]);
    }

    #[test]
    fn test_ollama_config_defaults() {
        let ollama = OllamaConfig {
            base_url: default_ollama_url(),
            embed_model: default_embed_model(),
            classify_model: default_classify_model(),
        };

        assert_eq!(ollama.base_url, "http://localhost:11434");
        assert_eq!(ollama.embed_model, "nomic-embed-text");
        assert_eq!(ollama.classify_model, "qwen2.5:7b");
    }

    #[test]
    fn test_classifier_config_defaults() {
        let classifier = ClassifierConfig {
            similar_count: default_similar_count(),
            min_confidence: default_min_confidence(),
        };

        assert_eq!(classifier.similar_count, 8);
        assert!((classifier.min_confidence - 0.7).abs() < 0.001);
    }

    #[test]
    fn test_get_config_dir() {
        let dir = get_config_dir();
        // Should end with "emailcl"
        assert!(dir.ends_with("emailcl"));
    }

    #[test]
    fn test_get_data_dir() {
        let dir = get_data_dir();
        // Should end with "emailcl"
        assert!(dir.ends_with("emailcl"));
    }

    #[test]
    fn test_get_config_path() {
        let path = get_config_path();
        // Should end with "config.toml"
        assert!(path.ends_with("config.toml"));
        // And parent should be emailcl
        assert!(path.parent().unwrap().ends_with("emailcl"));
    }
}
