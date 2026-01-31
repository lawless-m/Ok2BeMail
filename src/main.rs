mod auth;
mod classifier;
mod cli;
mod config;
mod db;
mod embedding;
mod error;
mod fetcher;
mod notifier;

use crate::auth::AuthManager;
use crate::classifier::Classifier;
use crate::cli::{CategoryAction, Cli, Commands};
use crate::config::{get_config_path, Config};
use crate::db::Database;
use crate::embedding::EmbeddingService;
use crate::error::Result;
use crate::fetcher::EmailFetcher;
use crate::notifier::Notifier;

use clap::Parser;
use std::io::{self, Write};
use tracing::{error, info, Level};
use tracing_subscriber::FmtSubscriber;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Set up logging
    let level = if cli.verbose { Level::DEBUG } else { Level::INFO };
    let subscriber = FmtSubscriber::builder()
        .with_max_level(level)
        .with_target(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("Failed to set subscriber");

    if let Err(e) = run(cli).await {
        error!("Error: {}", e);
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Init { client_id, tenant_id } => {
            cmd_init(client_id, tenant_id).await
        }
        Commands::Daemon => {
            cmd_daemon().await
        }
        Commands::Sync => {
            cmd_sync().await
        }
        Commands::Review => {
            cmd_review().await
        }
        Commands::List { unverified, category, limit } => {
            cmd_list(unverified, category, limit).await
        }
        Commands::Show { email_id } => {
            cmd_show(&email_id).await
        }
        Commands::Correct { email_id, category, importance } => {
            cmd_correct(&email_id, &category, importance).await
        }
        Commands::Verify { email_id } => {
            cmd_verify(&email_id).await
        }
        Commands::Category { action } => {
            cmd_category(action).await
        }
        Commands::Export { format, output } => {
            cmd_export(&format, output).await
        }
        Commands::Stats => {
            cmd_stats().await
        }
    }
}

async fn cmd_init(client_id: Option<String>, tenant_id: Option<String>) -> Result<()> {
    println!("Initializing emailcl...\n");

    // Load or create config
    let mut config = Config::default();

    // If config exists, load it
    if get_config_path().exists() {
        if let Ok(existing) = Config::load() {
            config = existing;
            println!("Loaded existing configuration.");
        }
    }

    // Update with provided values
    if let Some(id) = client_id {
        config.azure.client_id = id;
    }
    if let Some(id) = tenant_id {
        config.azure.tenant_id = id;
    }

    // Prompt for missing values
    if config.azure.client_id.is_empty() {
        print!("Azure Client ID: ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        config.azure.client_id = input.trim().to_string();
    }

    if config.azure.tenant_id.is_empty() {
        print!("Azure Tenant ID: ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        config.azure.tenant_id = input.trim().to_string();
    }

    // Save config
    config.save()?;
    println!("\nConfiguration saved to: {}", get_config_path().display());

    // Initialize database
    let db_path = config.expanded_db_path();
    let _db = Database::open(&db_path)?;
    println!("Database initialized at: {}", db_path.display());

    // Authenticate
    let auth = AuthManager::new(&config);
    auth.authenticate().await?;

    println!("\nInitialization complete!");
    println!("Run 'emailcl sync' to fetch and classify emails.");

    Ok(())
}

async fn cmd_daemon() -> Result<()> {
    let config = Config::load()?;
    let db = Database::open(config.expanded_db_path())?;
    let fetcher = EmailFetcher::new(&config);
    let embedding_service = EmbeddingService::new(&config);
    let classifier = Classifier::new(&config);
    let notifier = Notifier::new(&config.notify);

    info!("Starting daemon mode...");
    info!("Poll interval: {} seconds", config.sync.poll_interval_secs);

    // Check Ollama availability
    if !embedding_service.is_available().await {
        error!("Ollama is not available at {}. Please start Ollama first.", config.ollama.base_url);
        return Ok(());
    }

    loop {
        info!("Starting sync cycle...");

        // Fetch new emails
        match fetcher.sync(&db).await {
            Ok(count) => {
                if count > 0 {
                    info!("Fetched {} new emails", count);
                }
            }
            Err(e) => {
                error!("Fetch failed: {}", e);
            }
        }

        // Generate embeddings
        match embedding_service.process_unembedded_emails(&db, 50).await {
            Ok(count) => {
                if count > 0 {
                    info!("Embedded {} emails", count);
                }
            }
            Err(e) => {
                error!("Embedding failed: {}", e);
            }
        }

        // Classify and notify
        let emails = db.get_emails_without_label(50)?;
        for email in &emails {
            match classifier.classify_and_store(&db, email, &embedding_service).await {
                Ok(label) => {
                    info!(
                        "Classified '{}' as {} (importance: {})",
                        email.subject.as_deref().unwrap_or("(no subject)"),
                        label.category,
                        label.importance
                    );
                    if let Err(e) = notifier.notify(email, &label) {
                        error!("Notification failed: {}", e);
                    }
                }
                Err(e) => {
                    error!("Classification failed: {}", e);
                }
            }
        }

        info!("Sync cycle complete. Sleeping for {} seconds...", config.sync.poll_interval_secs);
        tokio::time::sleep(std::time::Duration::from_secs(config.sync.poll_interval_secs)).await;
    }
}

async fn cmd_sync() -> Result<()> {
    let config = Config::load()?;
    let db = Database::open(config.expanded_db_path())?;
    let fetcher = EmailFetcher::new(&config);
    let embedding_service = EmbeddingService::new(&config);
    let classifier = Classifier::new(&config);

    // Check Ollama availability
    if !embedding_service.is_available().await {
        error!("Ollama is not available at {}. Please start Ollama first.", config.ollama.base_url);
        return Ok(());
    }

    println!("Syncing emails...");

    // Fetch
    let fetched = fetcher.sync(&db).await?;
    println!("Fetched {} emails", fetched);

    // Embed
    let embedded = embedding_service.process_unembedded_emails(&db, 100).await?;
    println!("Embedded {} emails", embedded);

    // Classify
    let classified = classifier.process_unclassified_emails(&db, &embedding_service, 100).await?;
    println!("Classified {} emails", classified);

    println!("\nSync complete!");
    Ok(())
}

async fn cmd_review() -> Result<()> {
    let config = Config::load()?;
    let db = Database::open(config.expanded_db_path())?;

    println!("Interactive review mode. Commands:");
    println!("  n - next email");
    println!("  v - verify current classification");
    println!("  c <category> <importance> - correct classification");
    println!("  q - quit\n");

    let limit = 1;

    loop {
        let emails = db.list_emails_with_labels(true, None, limit)?;

        if emails.is_empty() {
            println!("No more unverified emails!");
            break;
        }

        let ewl = &emails[0];
        let email = &ewl.email;

        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("ID: {}", email.id);
        println!("From: {} <{}>",
            email.sender_name.as_deref().unwrap_or(""),
            email.sender_address
        );
        println!("Subject: {}", email.subject.as_deref().unwrap_or("(no subject)"));
        println!("Received: {}", email.received_at.format("%Y-%m-%d %H:%M"));

        if let Some(label) = &ewl.label {
            println!("\nCurrent classification:");
            println!("  Category: {}", label.category);
            println!("  Importance: {}", label.importance);
            println!("  Source: {}", label.source);
            if let Some(conf) = label.confidence {
                println!("  Confidence: {:.2}", conf);
            }
        } else {
            println!("\nNot yet classified");
        }

        println!("\nBody preview:");
        let body = email.body_text.as_deref().unwrap_or("");
        let preview = if body.len() > 500 { &body[..500] } else { body };
        println!("{}", preview);
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

        print!("> ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let parts: Vec<&str> = input.trim().split_whitespace().collect();

        if parts.is_empty() {
            continue;
        }

        match parts[0] {
            "n" | "next" => {
                continue;
            }
            "v" | "verify" => {
                db.verify_label(&email.id)?;
                println!("Classification verified.\n");
            }
            "c" | "correct" => {
                if parts.len() < 3 {
                    println!("Usage: c <category> <importance>");
                    continue;
                }
                let category = parts[1];
                let importance: i32 = parts[2].parse().unwrap_or(2);
                db.correct_label(&email.id, category, importance)?;
                println!("Classification corrected to {} (importance: {})\n", category, importance);
            }
            "q" | "quit" => {
                println!("Goodbye!");
                break;
            }
            _ => {
                println!("Unknown command. Use n, v, c, or q.");
            }
        }
    }

    Ok(())
}

async fn cmd_list(unverified: bool, category: Option<String>, limit: usize) -> Result<()> {
    let config = Config::load()?;
    let db = Database::open(config.expanded_db_path())?;

    let emails = db.list_emails_with_labels(unverified, category.as_deref(), limit)?;

    if emails.is_empty() {
        println!("No emails found.");
        return Ok(());
    }

    println!("{:<36} {:<12} {:<5} {:<8} {}", "ID", "Category", "Imp", "Source", "Subject");
    println!("{}", "─".repeat(100));

    for ewl in &emails {
        let email = &ewl.email;
        let (category, importance, source) = if let Some(label) = &ewl.label {
            (label.category.as_str(), label.importance.to_string(), label.source.as_str())
        } else {
            ("", "-".to_string(), "")
        };

        let subject = email.subject.as_deref().unwrap_or("(no subject)");
        let subject = if subject.len() > 40 {
            format!("{}...", &subject[..37])
        } else {
            subject.to_string()
        };

        println!("{:<36} {:<12} {:<5} {:<8} {}",
            &email.id[..36.min(email.id.len())],
            category,
            importance,
            source,
            subject
        );
    }

    Ok(())
}

async fn cmd_show(email_id: &str) -> Result<()> {
    let config = Config::load()?;
    let db = Database::open(config.expanded_db_path())?;

    let email = match db.get_email(email_id)? {
        Some(e) => e,
        None => {
            println!("Email not found: {}", email_id);
            return Ok(());
        }
    };

    let label = db.get_label(email_id)?;

    println!("ID: {}", email.id);
    println!("From: {} <{}>",
        email.sender_name.as_deref().unwrap_or(""),
        email.sender_address
    );
    println!("Subject: {}", email.subject.as_deref().unwrap_or("(no subject)"));
    println!("Received: {}", email.received_at.format("%Y-%m-%d %H:%M:%S"));
    println!("Has attachments: {}", email.has_attachments);
    println!("Read: {}", email.is_read);
    println!("Has embedding: {}", email.embedding.is_some());

    if let Some(l) = label {
        println!("\nClassification:");
        println!("  Category: {}", l.category);
        println!("  Importance: {}", l.importance);
        println!("  Source: {}", l.source);
        if let Some(conf) = l.confidence {
            println!("  Confidence: {:.2}", conf);
        }
    }

    println!("\nBody:");
    println!("{}", email.body_text.as_deref().unwrap_or("(no body)"));

    Ok(())
}

async fn cmd_correct(email_id: &str, category: &str, importance: i32) -> Result<()> {
    let config = Config::load()?;
    let db = Database::open(config.expanded_db_path())?;

    // Verify email exists
    if db.get_email(email_id)?.is_none() {
        println!("Email not found: {}", email_id);
        return Ok(());
    }

    let importance = importance.clamp(0, 4);
    db.correct_label(email_id, category, importance)?;

    println!("Classification updated:");
    println!("  Email: {}", email_id);
    println!("  Category: {}", category);
    println!("  Importance: {}", importance);
    println!("  Source: user");

    Ok(())
}

async fn cmd_verify(email_id: &str) -> Result<()> {
    let config = Config::load()?;
    let db = Database::open(config.expanded_db_path())?;

    // Verify email exists
    if db.get_email(email_id)?.is_none() {
        println!("Email not found: {}", email_id);
        return Ok(());
    }

    db.verify_label(email_id)?;
    println!("Classification verified for: {}", email_id);

    Ok(())
}

async fn cmd_category(action: CategoryAction) -> Result<()> {
    let config = Config::load()?;
    let db = Database::open(config.expanded_db_path())?;

    match action {
        CategoryAction::Add { name, description } => {
            db.add_category(&name, &description)?;
            println!("Category added: {} - {}", name, description);
        }
        CategoryAction::List => {
            let categories = db.list_categories()?;
            println!("{:<20} {}", "Name", "Description");
            println!("{}", "─".repeat(60));
            for (name, desc) in categories {
                println!("{:<20} {}", name, desc.unwrap_or_default());
            }
        }
    }

    Ok(())
}

async fn cmd_export(format: &str, output: Option<String>) -> Result<()> {
    let config = Config::load()?;
    let db = Database::open(config.expanded_db_path())?;

    let emails = db.list_emails_with_labels(false, None, 10000)?;

    let content = match format {
        "json" => {
            let data: Vec<_> = emails
                .iter()
                .map(|ewl| {
                    serde_json::json!({
                        "id": ewl.email.id,
                        "subject": ewl.email.subject,
                        "sender_address": ewl.email.sender_address,
                        "sender_name": ewl.email.sender_name,
                        "received_at": ewl.email.received_at.to_rfc3339(),
                        "category": ewl.label.as_ref().map(|l| &l.category),
                        "importance": ewl.label.as_ref().map(|l| l.importance),
                        "source": ewl.label.as_ref().map(|l| &l.source),
                    })
                })
                .collect();
            serde_json::to_string_pretty(&data)?
        }
        "csv" | _ => {
            let mut lines = vec!["id,subject,sender_address,sender_name,received_at,category,importance,source".to_string()];
            for ewl in &emails {
                let (category, importance, source) = if let Some(l) = &ewl.label {
                    (l.category.as_str(), l.importance.to_string(), l.source.as_str())
                } else {
                    ("", "".to_string(), "")
                };

                lines.push(format!(
                    "{},{},{},{},{},{},{},{}",
                    escape_csv(&ewl.email.id),
                    escape_csv(ewl.email.subject.as_deref().unwrap_or("")),
                    escape_csv(&ewl.email.sender_address),
                    escape_csv(ewl.email.sender_name.as_deref().unwrap_or("")),
                    ewl.email.received_at.to_rfc3339(),
                    category,
                    importance,
                    source
                ));
            }
            lines.join("\n")
        }
    };

    if let Some(path) = output {
        std::fs::write(&path, &content)?;
        println!("Exported {} emails to {}", emails.len(), path);
    } else {
        println!("{}", content);
    }

    Ok(())
}

fn escape_csv(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

async fn cmd_stats() -> Result<()> {
    let config = Config::load()?;
    let db = Database::open(config.expanded_db_path())?;

    let total_emails = db.email_count()?;
    let total_labeled = db.labeled_count()?;
    let user_verified = db.user_verified_count()?;

    println!("Email Classifier Statistics");
    println!("═══════════════════════════");
    println!("Total emails:     {}", total_emails);
    println!("Labeled:          {} ({:.1}%)", total_labeled,
        if total_emails > 0 { total_labeled as f64 / total_emails as f64 * 100.0 } else { 0.0 });
    println!("User verified:    {} ({:.1}%)", user_verified,
        if total_labeled > 0 { user_verified as f64 / total_labeled as f64 * 100.0 } else { 0.0 });
    println!("\nDatabase: {}", config.expanded_db_path().display());
    println!("Config: {}", get_config_path().display());

    Ok(())
}
