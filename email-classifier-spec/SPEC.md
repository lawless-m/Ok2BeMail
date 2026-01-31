# Email Classifier with Local LLM Analysis

## Overview

A Rust application that fetches corporate email from Office 365 via Microsoft Graph API, stores it locally, and uses a local Qwen LLM (via Ollama) to classify emails and notify the user of important items. Classification improves over time through user feedback using retrieval-augmented few-shot learning.

## Goals

- **Privacy**: All analysis happens on-premises. No email content leaves the local network.
- **Classification**: Automatically categorise incoming emails (HR, system alerts, external contacts, junk, etc.)
- **Notification**: Alert user to important/urgent emails based on classification and learned preferences.
- **Learning**: Improve classification accuracy over time through user corrections without full model retraining.

## Hardware

- **GPU**: NVIDIA RTX 3090 (24GB VRAM)
- **CPU**: Dual Xeon
- **RAM**: 64GB
- **OS**: Windows (work machine)
- **Inference**: Ollama (already running)

## Non-Goals (Phase 1)

- Attachment content analysis
- Calendar/contacts sync
- Email sending/replying
- Full RLHF training loop

---

## Architecture

```
┌─────────────────┐     ┌──────────────────┐     ┌─────────────────┐
│  Microsoft 365  │────▶│  Email Fetcher   │────▶│  Local Storage  │
│  (Graph API)    │     │  (Rust)          │     │  (SQLite)       │
└─────────────────┘     └──────────────────┘     └────────┬────────┘
                                                          │
                        ┌──────────────────┐              │
                        │  Embedding       │◀─────────────┤
                        │  Service         │              │
                        │  (Ollama)        │              │
                        └────────┬─────────┘              │
                                 │                        │
                                 ▼                        │
                        ┌──────────────────┐              │
                        │  Vector Store    │              │
                        │  (SQLite + vec)  │              │
                        └────────┬─────────┘              │
                                 │                        │
                                 ▼                        ▼
                        ┌──────────────────┐     ┌─────────────────┐
                        │  Classifier      │────▶│  Notifier       │
                        │  (Qwen/Ollama)   │     │  (Desktop)      │
                        └────────┬─────────┘     └─────────────────┘
                                 │
                                 ▼
                        ┌──────────────────┐
                        │  Feedback UI     │
                        │  (CLI/TUI)       │
                        └──────────────────┘
```

---

## Components

### 1. Authentication Module

**Responsibility**: Handle Microsoft OAuth2 authentication and token management.

**Flow**:
1. On first run, initiate device code flow
2. User authenticates in browser, grants `Mail.Read` permission
3. Store tokens locally (encrypted)
4. Refresh tokens automatically before expiry

**Configuration** (via config file or env vars):
- `AZURE_CLIENT_ID` - Application (client) ID from Azure AD app registration
- `AZURE_TENANT_ID` - Directory (tenant) ID (or "common" for multi-tenant)

**Token storage**: Encrypted file in user config directory. Use `keyring` crate or similar for OS credential storage if available.

**Scopes required**:
- `Mail.Read` (delegated) - Read user's mail
- `offline_access` - Refresh tokens

### 2. Email Fetcher

**Responsibility**: Pull emails from Graph API, handle pagination and delta sync.

**Endpoints**:
```
GET https://graph.microsoft.com/v1.0/me/mailFolders/inbox/messages
GET https://graph.microsoft.com/v1.0/me/mailFolders/inbox/messages/delta
```

**Fields to fetch** (use `$select` to limit):
- `id`
- `subject`
- `bodyPreview` (first 255 chars)
- `body.content` (full body)
- `body.contentType` (html or text)
- `from.emailAddress.address`
- `from.emailAddress.name`
- `receivedDateTime`
- `parentFolderId`
- `hasAttachments`
- `isRead`
- `importance`

**Do NOT fetch**:
- `attachments` - not needed for classification

**Sync strategy**:
- Initial: Full sync of last N days (configurable, default 30)
- Ongoing: Delta queries to fetch only new/changed messages
- Store delta link for next sync

**Rate limiting**: Graph API has generous limits but implement exponential backoff on 429 responses.

### 3. Local Storage (SQLite)

**Responsibility**: Persistent storage for emails, embeddings, labels, and sync state.

**Schema**:

```sql
-- Core email storage
CREATE TABLE emails (
    id TEXT PRIMARY KEY,              -- Graph API message ID
    subject TEXT,
    body_text TEXT,                   -- Plain text extracted from body
    body_html TEXT,                   -- Original HTML if present
    sender_address TEXT NOT NULL,
    sender_name TEXT,
    received_at TEXT NOT NULL,        -- ISO 8601
    folder_id TEXT,
    has_attachments INTEGER DEFAULT 0,
    is_read INTEGER DEFAULT 0,
    importance TEXT,                  -- low, normal, high
    fetched_at TEXT NOT NULL,         -- When we fetched it
    embedding BLOB,                   -- Vector as packed f32s
    created_at TEXT DEFAULT CURRENT_TIMESTAMP
);

-- Classification labels
CREATE TABLE labels (
    email_id TEXT PRIMARY KEY REFERENCES emails(id),
    category TEXT NOT NULL,           -- hr, system_alert, external, junk, etc.
    importance INTEGER DEFAULT 0,     -- 0=ignore, 1=low, 2=normal, 3=high, 4=urgent
    source TEXT NOT NULL,             -- 'model' or 'user'
    confidence REAL,                  -- Model confidence if source='model'
    created_at TEXT DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT DEFAULT CURRENT_TIMESTAMP
);

-- Sync state
CREATE TABLE sync_state (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT DEFAULT CURRENT_TIMESTAMP
);

-- Indexes
CREATE INDEX idx_emails_received ON emails(received_at DESC);
CREATE INDEX idx_emails_sender ON emails(sender_address);
CREATE INDEX idx_labels_category ON labels(category);
CREATE INDEX idx_labels_source ON labels(source);
```

**Vector storage options**:

Option A: Store embeddings as BLOB in SQLite, use `sqlite-vec` extension for similarity search.

Option B: Separate vector DB (qdrant, milvus). More complexity but better performance at scale.

**Recommendation**: Start with sqlite-vec. Thousands of emails is trivial. Move to dedicated vector DB if needed.

### 4. Embedding Service

**Responsibility**: Generate vector embeddings for email content.

**Model**: `nomic-embed-text` via Ollama (768 dimensions, good quality, fast)

Alternative: `mxbai-embed-large` (1024 dims, slightly better quality)

**Input**: Concatenated string of subject + sender + body (truncated if needed)

**Format**:
```
From: {sender_name} <{sender_address}>
Subject: {subject}

{body_text}
```

**Ollama API**:
```
POST http://localhost:11434/api/embeddings
{
    "model": "nomic-embed-text",
    "prompt": "From: ..."
}
```

**Batch processing**: Embed multiple emails per request if Ollama supports it, otherwise parallelise requests.

### 5. Classifier

**Responsibility**: Classify emails using retrieval-augmented few-shot prompting.

**Flow**:
1. Email arrives (or batch of emails to classify)
2. Generate embedding for new email
3. Query vector store for K most similar emails that have user-verified labels
4. Construct few-shot prompt with similar examples
5. Send to Qwen for classification
6. Parse response, store label

**Model**: `qwen2.5:7b` or `qwen2.5:14b` via Ollama

**Prompt template**:
```
You are an email classifier. Based on the examples below, classify the new email.

Categories:
- hr: Human resources communications, policies, benefits
- system_alert: Automated system notifications, monitoring alerts, CI/CD
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

Examples of previously classified emails:

{examples}

Now classify this email:

From: {sender_name} <{sender_address}>
Subject: {subject}
Received: {received_at}

{body_text}

Respond with JSON only:
{"category": "...", "importance": N, "reason": "brief explanation"}
```

**Example format** (for few-shot):
```
---
From: HR Team <hr@company.com>
Subject: Updated Holiday Policy
Category: hr
Importance: 2
---
From: alerts@monitoring.internal
Subject: CRITICAL: Database connection pool exhausted
Category: system_alert
Importance: 4
---
```

**Retrieval parameters**:
- K = 5-10 similar emails (tune based on results)
- Only include emails where `labels.source = 'user'` (user-verified) for highest quality
- Fall back to `source = 'model'` with high confidence if insufficient user labels

**Ollama API**:
```
POST http://localhost:11434/api/generate
{
    "model": "qwen2.5:7b",
    "prompt": "...",
    "stream": false,
    "format": "json"
}
```

### 6. Feedback Handler

**Responsibility**: Accept user corrections and update labels.

**Interface**: CLI or TUI (can add web UI later)

**Commands**:
```
# List recent classifications
emailcl list [--unverified] [--category X]

# Correct a classification
emailcl correct <email_id> --category <cat> --importance <N>

# Mark model classification as verified (user agrees)
emailcl verify <email_id>

# Show classification for specific email
emailcl show <email_id>

# Add new category
emailcl category add <name> --description "..."

# List categories
emailcl category list
```

**On correction**:
1. Update `labels` table: set new category/importance, `source = 'user'`
2. That email now becomes a high-quality example for future few-shot retrieval
3. No retraining needed - retrieval handles it automatically

### 7. Notifier

**Responsibility**: Alert user to important emails based on classification.

**Rules** (configurable):
```toml
[[notify.rules]]
category = "system_alert"
min_importance = 3
action = "desktop"

[[notify.rules]]
category = "external"
min_importance = 2
action = "desktop"

[[notify.rules]]
category = "hr"
min_importance = 3
action = "desktop"

# Default: no notification for junk, low-importance items
```

**Desktop notifications**: Use `notify-rust` crate on Linux, `winrt-notification` on Windows.

**Notification content**:
```
[Category] Subject
From: sender
```

---

## Configuration

**Config file**: `~/.config/emailcl/config.toml` (Linux) or `%APPDATA%\emailcl\config.toml` (Windows)

```toml
[azure]
client_id = "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
tenant_id = "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"  # or "common"

[sync]
initial_days = 30           # How far back to fetch on first sync
poll_interval_secs = 300    # How often to check for new mail
folders = ["inbox"]         # Which folders to sync

[ollama]
base_url = "http://localhost:11434"
embed_model = "nomic-embed-text"
classify_model = "qwen2.5:7b"

[classifier]
similar_count = 8           # Number of similar emails for few-shot
min_confidence = 0.7        # Below this, flag for review

[storage]
db_path = "~/.local/share/emailcl/emails.db"

[notify]
enabled = true
# Rules as above
```

---

## Startup / Run Modes

```bash
# First run - authenticate and initial sync
emailcl init

# Run daemon (fetch, classify, notify)
emailcl daemon

# One-shot sync and classify
emailcl sync

# Interactive mode for reviewing/correcting
emailcl review

# Export classifications (for analysis)
emailcl export --format csv
```

---

## Error Handling

- **Auth failures**: Clear tokens, re-initiate device code flow
- **Network errors**: Exponential backoff, retry
- **Ollama unavailable**: Queue emails for classification, retry when available
- **Parse errors**: Log failed classification, flag for manual review
- **Rate limits**: Respect Retry-After header

---

## Security Considerations

- Tokens stored encrypted or in OS keychain
- Database file permissions restricted (600)
- No email content transmitted except to local Ollama
- Config file should not contain secrets (use env vars or keychain for sensitive values)

---

## Future Enhancements (Phase 2+)

- **LoRA fine-tuning**: Periodically train adapter on accumulated corrections
- **Attachment analysis**: OCR for images, text extraction for PDFs/docs
- **Web UI**: Browser-based review interface
- **Rules engine**: User-defined rules (e.g., "always mark emails from X as urgent")
- **Thread analysis**: Classify entire conversations, not just individual emails
- **Sentiment/tone detection**: Flag angry or escalating threads
- **Action suggestions**: "This looks like it needs a response"

---

## Crate Recommendations

| Purpose | Crate |
|---------|-------|
| HTTP client | `reqwest` |
| Async runtime | `tokio` |
| OAuth2 | `oauth2` |
| SQLite | `rusqlite` with `bundled` feature |
| Vector search | `sqlite-vec` or `qdrant-client` |
| JSON | `serde`, `serde_json` |
| Config | `config` or `toml` |
| CLI | `clap` |
| TUI | `ratatui` (if doing TUI) |
| Notifications | `notify-rust` (Linux), `winrt-notification` (Windows) |
| Logging | `tracing`, `tracing-subscriber` |
| Error handling | `anyhow`, `thiserror` |
| HTML to text | `html2text` |

---

## Getting Started

1. Register app in Azure AD (see AUTH_SETUP.md)
2. Install Ollama, pull required models
3. Build and run `emailcl init`
4. Run `emailcl daemon` or add to system startup
5. Review initial classifications with `emailcl review`
6. Correct misclassifications to improve future accuracy
