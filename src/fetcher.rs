use crate::auth::AuthManager;
use crate::config::Config;
use crate::db::{Database, Email};
use crate::error::{EmailClError, Result};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use tracing::{debug, info, warn};

const GRAPH_BASE_URL: &str = "https://graph.microsoft.com/v1.0";
const PAGE_SIZE: u32 = 50;

#[derive(Debug, Deserialize)]
struct GraphResponse<T> {
    value: Vec<T>,
    #[serde(rename = "@odata.nextLink")]
    next_link: Option<String>,
    #[serde(rename = "@odata.deltaLink")]
    delta_link: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GraphMessage {
    id: String,
    subject: Option<String>,
    #[serde(rename = "bodyPreview")]
    body_preview: Option<String>,
    body: Option<MessageBody>,
    from: Option<EmailRecipient>,
    #[serde(rename = "receivedDateTime")]
    received_date_time: String,
    #[serde(rename = "parentFolderId")]
    parent_folder_id: Option<String>,
    #[serde(rename = "hasAttachments")]
    has_attachments: Option<bool>,
    #[serde(rename = "isRead")]
    is_read: Option<bool>,
    importance: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MessageBody {
    content: Option<String>,
    #[serde(rename = "contentType")]
    content_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EmailRecipient {
    #[serde(rename = "emailAddress")]
    email_address: EmailAddress,
}

#[derive(Debug, Deserialize)]
struct EmailAddress {
    address: String,
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GraphErrorResponse {
    error: GraphError,
}

#[derive(Debug, Deserialize)]
struct GraphError {
    code: String,
    message: String,
}

pub struct EmailFetcher {
    auth_manager: AuthManager,
    http_client: reqwest::Client,
    initial_days: u32,
}

impl EmailFetcher {
    pub fn new(config: &Config) -> Self {
        EmailFetcher {
            auth_manager: AuthManager::new(config),
            http_client: reqwest::Client::new(),
            initial_days: config.sync.initial_days,
        }
    }

    pub async fn sync(&self, db: &Database) -> Result<usize> {
        let access_token = self.auth_manager.get_valid_token().await?;

        // Check if we have a delta link for incremental sync
        if let Some(delta_link) = db.get_sync_state("delta_link")? {
            info!("Performing delta sync...");
            self.delta_sync(db, &access_token, &delta_link).await
        } else {
            info!("Performing initial sync (last {} days)...", self.initial_days);
            self.initial_sync(db, &access_token).await
        }
    }

    async fn initial_sync(&self, db: &Database, access_token: &str) -> Result<usize> {
        let since = Utc::now() - Duration::days(self.initial_days as i64);
        let filter = format!("receivedDateTime ge {}", since.format("%Y-%m-%dT%H:%M:%SZ"));

        let select = [
            "id",
            "subject",
            "bodyPreview",
            "body",
            "from",
            "receivedDateTime",
            "parentFolderId",
            "hasAttachments",
            "isRead",
            "importance",
        ]
        .join(",");

        let url = format!(
            "{}/me/mailFolders/inbox/messages?$select={}&$filter={}&$top={}&$orderby=receivedDateTime desc",
            GRAPH_BASE_URL,
            urlencoding::encode(&select),
            urlencoding::encode(&filter),
            PAGE_SIZE
        );

        let (count, delta_link) = self.fetch_messages(db, access_token, &url).await?;

        // Now get the delta link for future syncs
        if delta_link.is_none() {
            // Make a delta request to get the initial delta link
            let delta_url = format!(
                "{}/me/mailFolders/inbox/messages/delta?$select={}",
                GRAPH_BASE_URL,
                urlencoding::encode(&select)
            );
            let (_, new_delta_link) = self.fetch_messages(db, access_token, &delta_url).await?;
            if let Some(link) = new_delta_link {
                db.set_sync_state("delta_link", &link)?;
            }
        } else if let Some(link) = delta_link {
            db.set_sync_state("delta_link", &link)?;
        }

        Ok(count)
    }

    async fn delta_sync(
        &self,
        db: &Database,
        access_token: &str,
        delta_link: &str,
    ) -> Result<usize> {
        let (count, new_delta_link) = self.fetch_messages(db, access_token, delta_link).await?;

        if let Some(link) = new_delta_link {
            db.set_sync_state("delta_link", &link)?;
        }

        Ok(count)
    }

    async fn fetch_messages(
        &self,
        db: &Database,
        access_token: &str,
        initial_url: &str,
    ) -> Result<(usize, Option<String>)> {
        let mut url = initial_url.to_string();
        let mut count = 0;
        let mut delta_link = None;
        let mut retry_count = 0;
        const MAX_RETRIES: u32 = 4;

        loop {
            debug!("Fetching: {}", url);

            let response = self
                .http_client
                .get(&url)
                .bearer_auth(access_token)
                .send()
                .await?;

            let status = response.status();

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                // Handle rate limiting
                let retry_after = response
                    .headers()
                    .get("Retry-After")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(60);

                warn!("Rate limited. Waiting {} seconds...", retry_after);
                tokio::time::sleep(std::time::Duration::from_secs(retry_after)).await;
                continue;
            }

            if !status.is_success() {
                let body = response.text().await?;

                // Check for network errors and retry
                if retry_count < MAX_RETRIES {
                    retry_count += 1;
                    let delay = 2u64.pow(retry_count);
                    warn!(
                        "Request failed (attempt {}), retrying in {} seconds: {}",
                        retry_count, delay, body
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                    continue;
                }

                if let Ok(error) = serde_json::from_str::<GraphErrorResponse>(&body) {
                    return Err(EmailClError::GraphApi(format!(
                        "{}: {}",
                        error.error.code, error.error.message
                    )));
                }
                return Err(EmailClError::GraphApi(format!("HTTP {}: {}", status, body)));
            }

            retry_count = 0; // Reset on success

            let graph_response: GraphResponse<GraphMessage> = response.json().await?;

            for msg in graph_response.value {
                let email = convert_graph_message(msg)?;
                db.insert_email(&email)?;
                count += 1;
            }

            if let Some(next) = graph_response.next_link {
                url = next;
            } else {
                delta_link = graph_response.delta_link;
                break;
            }
        }

        info!("Fetched {} messages", count);
        Ok((count, delta_link))
    }
}

fn convert_graph_message(msg: GraphMessage) -> Result<Email> {
    let sender_address = msg
        .from
        .as_ref()
        .map(|f| f.email_address.address.clone())
        .unwrap_or_else(|| "unknown@unknown".to_string());

    let sender_name = msg.from.as_ref().and_then(|f| f.email_address.name.clone());

    let received_at = DateTime::parse_from_rfc3339(&msg.received_date_time)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| EmailClError::GraphApi(format!("Invalid date format: {}", e)))?;

    // Extract body text
    let (body_text, body_html) = if let Some(body) = &msg.body {
        let content = body.content.clone();
        let content_type = body.content_type.as_deref().unwrap_or("text");

        if content_type.to_lowercase() == "html" {
            let text = content
                .as_ref()
                .map(|html| html2text::from_read(html.as_bytes(), 80));
            (text, content)
        } else {
            (content.clone(), None)
        }
    } else {
        (msg.body_preview.clone(), None)
    };

    Ok(Email {
        id: msg.id,
        subject: msg.subject,
        body_text,
        body_html,
        sender_address,
        sender_name,
        received_at,
        folder_id: msg.parent_folder_id,
        has_attachments: msg.has_attachments.unwrap_or(false),
        is_read: msg.is_read.unwrap_or(false),
        importance: msg.importance,
        fetched_at: Utc::now(),
        embedding: None,
    })
}
