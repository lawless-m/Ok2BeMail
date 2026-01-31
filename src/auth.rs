use crate::config::{get_config_dir, Config};
use crate::error::{EmailClError, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{debug, info};

const GRAPH_SCOPES: &[&str] = &["Mail.Read", "offline_access"];
const TOKEN_FILE: &str = "tokens.json";
const SERVICE_NAME: &str = "emailcl";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenData {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub token_type: String,
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: i64,
    interval: u64,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: i64,
    token_type: String,
}

#[derive(Debug, Deserialize)]
struct ErrorResponse {
    error: String,
    error_description: Option<String>,
}

pub struct AuthManager {
    client_id: String,
    tenant_id: String,
    http_client: reqwest::Client,
}

impl AuthManager {
    pub fn new(config: &Config) -> Self {
        AuthManager {
            client_id: config.azure.client_id.clone(),
            tenant_id: config.azure.tenant_id.clone(),
            http_client: reqwest::Client::new(),
        }
    }

    fn token_endpoint(&self) -> String {
        format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
            self.tenant_id
        )
    }

    fn device_code_endpoint(&self) -> String {
        format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/devicecode",
            self.tenant_id
        )
    }

    fn token_file_path() -> PathBuf {
        get_config_dir().join(TOKEN_FILE)
    }

    pub async fn get_valid_token(&self) -> Result<String> {
        // Try to load existing token
        if let Some(token_data) = self.load_token()? {
            // Check if token is still valid (with 5 minute buffer)
            if token_data.expires_at > Utc::now() + Duration::minutes(5) {
                debug!("Using cached access token");
                return Ok(token_data.access_token);
            }

            // Try to refresh the token
            if let Some(refresh_token) = &token_data.refresh_token {
                info!("Access token expired, refreshing...");
                match self.refresh_token(refresh_token).await {
                    Ok(new_token) => {
                        self.save_token(&new_token)?;
                        return Ok(new_token.access_token);
                    }
                    Err(e) => {
                        info!("Token refresh failed: {}. Re-authentication required.", e);
                    }
                }
            }
        }

        // Need to authenticate
        Err(EmailClError::Auth(
            "No valid token. Run 'emailcl init' to authenticate.".to_string(),
        ))
    }

    pub async fn authenticate(&self) -> Result<TokenData> {
        info!("Starting device code authentication flow...");

        // Request device code
        let scope = GRAPH_SCOPES.join(" ");
        let params = [("client_id", &self.client_id), ("scope", &scope)];

        let response = self
            .http_client
            .post(&self.device_code_endpoint())
            .form(&params)
            .send()
            .await?;

        if !response.status().is_success() {
            let error: ErrorResponse = response.json().await?;
            return Err(EmailClError::Auth(format!(
                "Device code request failed: {} - {}",
                error.error,
                error.error_description.unwrap_or_default()
            )));
        }

        let device_code: DeviceCodeResponse = response.json().await?;

        // Display instructions to user
        println!("\nTo sign in, open your browser and go to:");
        println!("\n  {}", device_code.verification_uri);
        println!("\nEnter the code: {}\n", device_code.user_code);
        println!("Waiting for authentication...");

        // Poll for token
        let token = self
            .poll_for_token(&device_code.device_code, device_code.interval, device_code.expires_in)
            .await?;

        // Save token
        self.save_token(&token)?;

        info!("Authentication successful!");
        Ok(token)
    }

    async fn poll_for_token(
        &self,
        device_code: &str,
        interval: u64,
        expires_in: i64,
    ) -> Result<TokenData> {
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(expires_in as u64);

        loop {
            if start.elapsed() > timeout {
                return Err(EmailClError::Auth("Device code expired".to_string()));
            }

            tokio::time::sleep(std::time::Duration::from_secs(interval)).await;

            let params = [
                ("client_id", self.client_id.as_str()),
                ("device_code", device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ];

            let response = self
                .http_client
                .post(&self.token_endpoint())
                .form(&params)
                .send()
                .await?;

            let status = response.status();
            let body = response.text().await?;

            if status.is_success() {
                let token_response: TokenResponse = serde_json::from_str(&body)?;
                let expires_at = Utc::now() + Duration::seconds(token_response.expires_in);

                return Ok(TokenData {
                    access_token: token_response.access_token,
                    refresh_token: token_response.refresh_token,
                    expires_at,
                    token_type: token_response.token_type,
                });
            }

            let error: ErrorResponse = serde_json::from_str(&body)?;
            match error.error.as_str() {
                "authorization_pending" => {
                    // User hasn't completed auth yet, keep polling
                    continue;
                }
                "slow_down" => {
                    // Need to slow down polling
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
                "expired_token" => {
                    return Err(EmailClError::Auth("Device code expired".to_string()));
                }
                _ => {
                    return Err(EmailClError::Auth(format!(
                        "Token request failed: {} - {}",
                        error.error,
                        error.error_description.unwrap_or_default()
                    )));
                }
            }
        }
    }

    async fn refresh_token(&self, refresh_token: &str) -> Result<TokenData> {
        let scope = GRAPH_SCOPES.join(" ");
        let params = [
            ("client_id", self.client_id.as_str()),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
            ("scope", scope.as_str()),
        ];

        let response = self
            .http_client
            .post(&self.token_endpoint())
            .form(&params)
            .send()
            .await?;

        if !response.status().is_success() {
            let error: ErrorResponse = response.json().await?;
            return Err(EmailClError::Token(format!(
                "Token refresh failed: {} - {}",
                error.error,
                error.error_description.unwrap_or_default()
            )));
        }

        let token_response: TokenResponse = response.json().await?;
        let expires_at = Utc::now() + Duration::seconds(token_response.expires_in);

        Ok(TokenData {
            access_token: token_response.access_token,
            refresh_token: token_response.refresh_token,
            expires_at,
            token_type: token_response.token_type,
        })
    }

    fn load_token(&self) -> Result<Option<TokenData>> {
        // First, try OS keyring
        if let Ok(entry) = keyring::Entry::new(SERVICE_NAME, "tokens") {
            if let Ok(json) = entry.get_password() {
                if let Ok(token) = serde_json::from_str(&json) {
                    debug!("Loaded token from keyring");
                    return Ok(Some(token));
                }
            }
        }

        // Fall back to file
        let path = Self::token_file_path();
        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            let token: TokenData = serde_json::from_str(&content)?;
            debug!("Loaded token from file");
            return Ok(Some(token));
        }

        Ok(None)
    }

    fn save_token(&self, token: &TokenData) -> Result<()> {
        let json = serde_json::to_string(token)?;

        // Try to save to OS keyring first
        if let Ok(entry) = keyring::Entry::new(SERVICE_NAME, "tokens") {
            if entry.set_password(&json).is_ok() {
                debug!("Saved token to keyring");
                return Ok(());
            }
        }

        // Fall back to file
        let path = Self::token_file_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        std::fs::write(&path, &json)?;

        // Set restrictive permissions on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(&path, perms)?;
        }

        debug!("Saved token to file");
        Ok(())
    }

    pub fn clear_tokens(&self) -> Result<()> {
        // Clear from keyring
        if let Ok(entry) = keyring::Entry::new(SERVICE_NAME, "tokens") {
            let _ = entry.delete_credential();
        }

        // Clear file
        let path = Self::token_file_path();
        if path.exists() {
            std::fs::remove_file(&path)?;
        }

        info!("Cleared stored tokens");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AzureConfig, Config};

    fn create_test_config(client_id: &str, tenant_id: &str) -> Config {
        Config {
            azure: AzureConfig {
                client_id: client_id.to_string(),
                tenant_id: tenant_id.to_string(),
            },
            ..Config::default()
        }
    }

    #[test]
    fn test_auth_manager_creation() {
        let config = create_test_config("test-client-id", "test-tenant-id");
        let auth = AuthManager::new(&config);

        assert_eq!(auth.client_id, "test-client-id");
        assert_eq!(auth.tenant_id, "test-tenant-id");
    }

    #[test]
    fn test_token_endpoint() {
        let config = create_test_config("client-123", "tenant-456");
        let auth = AuthManager::new(&config);

        let endpoint = auth.token_endpoint();
        assert_eq!(
            endpoint,
            "https://login.microsoftonline.com/tenant-456/oauth2/v2.0/token"
        );
    }

    #[test]
    fn test_device_code_endpoint() {
        let config = create_test_config("client-123", "tenant-456");
        let auth = AuthManager::new(&config);

        let endpoint = auth.device_code_endpoint();
        assert_eq!(
            endpoint,
            "https://login.microsoftonline.com/tenant-456/oauth2/v2.0/devicecode"
        );
    }

    #[test]
    fn test_token_endpoint_with_different_tenants() {
        let tenants = [
            "common",
            "organizations",
            "consumers",
            "my-specific-tenant-id",
            "12345678-1234-1234-1234-123456789012",
        ];

        for tenant in tenants {
            let config = create_test_config("client", tenant);
            let auth = AuthManager::new(&config);

            let endpoint = auth.token_endpoint();
            assert!(endpoint.contains(tenant));
            assert!(endpoint.starts_with("https://login.microsoftonline.com/"));
            assert!(endpoint.ends_with("/oauth2/v2.0/token"));
        }
    }

    #[test]
    fn test_token_data_serialization() {
        let token = TokenData {
            access_token: "eyJ0eXAi...access".to_string(),
            refresh_token: Some("eyJ0eXAi...refresh".to_string()),
            expires_at: Utc::now() + Duration::hours(1),
            token_type: "Bearer".to_string(),
        };

        let json = serde_json::to_string(&token).expect("Failed to serialize");
        assert!(json.contains("eyJ0eXAi...access"));
        assert!(json.contains("eyJ0eXAi...refresh"));
        assert!(json.contains("Bearer"));
    }

    #[test]
    fn test_token_data_deserialization() {
        let json = r#"{
            "access_token": "test-access-token",
            "refresh_token": "test-refresh-token",
            "expires_at": "2025-01-01T12:00:00Z",
            "token_type": "Bearer"
        }"#;

        let token: TokenData = serde_json::from_str(json).expect("Failed to deserialize");
        assert_eq!(token.access_token, "test-access-token");
        assert_eq!(token.refresh_token, Some("test-refresh-token".to_string()));
        assert_eq!(token.token_type, "Bearer");
    }

    #[test]
    fn test_token_data_roundtrip() {
        let original = TokenData {
            access_token: "access-token-123".to_string(),
            refresh_token: Some("refresh-token-456".to_string()),
            expires_at: DateTime::parse_from_rfc3339("2025-06-15T10:30:00Z")
                .unwrap()
                .with_timezone(&Utc),
            token_type: "Bearer".to_string(),
        };

        let json = serde_json::to_string(&original).expect("Failed to serialize");
        let deserialized: TokenData = serde_json::from_str(&json).expect("Failed to deserialize");

        assert_eq!(deserialized.access_token, original.access_token);
        assert_eq!(deserialized.refresh_token, original.refresh_token);
        assert_eq!(deserialized.token_type, original.token_type);
        assert_eq!(deserialized.expires_at, original.expires_at);
    }

    #[test]
    fn test_token_data_without_refresh_token() {
        let json = r#"{
            "access_token": "only-access",
            "refresh_token": null,
            "expires_at": "2025-01-01T12:00:00Z",
            "token_type": "Bearer"
        }"#;

        let token: TokenData = serde_json::from_str(json).expect("Failed to deserialize");
        assert_eq!(token.access_token, "only-access");
        assert!(token.refresh_token.is_none());
    }

    #[test]
    fn test_graph_scopes() {
        assert!(GRAPH_SCOPES.contains(&"Mail.Read"));
        assert!(GRAPH_SCOPES.contains(&"offline_access"));
        assert_eq!(GRAPH_SCOPES.len(), 2);
    }

    #[test]
    fn test_token_file_path() {
        let path = AuthManager::token_file_path();

        // Should end with tokens.json
        assert!(path.ends_with("tokens.json"));
        // Should be in emailcl config directory
        assert!(path.parent().unwrap().ends_with("emailcl"));
    }

    #[test]
    fn test_device_code_response_deserialization() {
        let json = r#"{
            "device_code": "device123",
            "user_code": "ABCD-EFGH",
            "verification_uri": "https://microsoft.com/devicelogin",
            "expires_in": 900,
            "interval": 5
        }"#;

        let response: DeviceCodeResponse = serde_json::from_str(json).expect("Failed to deserialize");
        assert_eq!(response.device_code, "device123");
        assert_eq!(response.user_code, "ABCD-EFGH");
        assert_eq!(response.verification_uri, "https://microsoft.com/devicelogin");
        assert_eq!(response.expires_in, 900);
        assert_eq!(response.interval, 5);
    }

    #[test]
    fn test_token_response_deserialization() {
        let json = r#"{
            "access_token": "access123",
            "refresh_token": "refresh456",
            "expires_in": 3600,
            "token_type": "Bearer"
        }"#;

        let response: TokenResponse = serde_json::from_str(json).expect("Failed to deserialize");
        assert_eq!(response.access_token, "access123");
        assert_eq!(response.refresh_token, Some("refresh456".to_string()));
        assert_eq!(response.expires_in, 3600);
        assert_eq!(response.token_type, "Bearer");
    }

    #[test]
    fn test_error_response_deserialization() {
        let json = r#"{
            "error": "authorization_pending",
            "error_description": "User has not completed authentication"
        }"#;

        let response: ErrorResponse = serde_json::from_str(json).expect("Failed to deserialize");
        assert_eq!(response.error, "authorization_pending");
        assert_eq!(
            response.error_description,
            Some("User has not completed authentication".to_string())
        );
    }

    #[test]
    fn test_error_response_without_description() {
        let json = r#"{
            "error": "expired_token"
        }"#;

        let response: ErrorResponse = serde_json::from_str(json).expect("Failed to deserialize");
        assert_eq!(response.error, "expired_token");
        assert!(response.error_description.is_none());
    }

    #[test]
    fn test_token_expiry_check() {
        // Token that expires in 10 minutes
        let valid_token = TokenData {
            access_token: "valid".to_string(),
            refresh_token: None,
            expires_at: Utc::now() + Duration::minutes(10),
            token_type: "Bearer".to_string(),
        };

        // Token expires in more than 5 minutes, so it's valid
        assert!(valid_token.expires_at > Utc::now() + Duration::minutes(5));

        // Token that expires in 3 minutes (should trigger refresh)
        let expiring_token = TokenData {
            access_token: "expiring".to_string(),
            refresh_token: None,
            expires_at: Utc::now() + Duration::minutes(3),
            token_type: "Bearer".to_string(),
        };

        // Token expires in less than 5 minutes, so it should be refreshed
        assert!(expiring_token.expires_at <= Utc::now() + Duration::minutes(5));
    }
}
