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
