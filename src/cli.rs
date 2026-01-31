use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "emailcl")]
#[command(about = "Email classifier with local LLM analysis")]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Enable verbose logging
    #[arg(short, long, global = true)]
    pub verbose: bool,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize: set up configuration and authenticate
    Init {
        /// Azure AD client ID
        #[arg(long)]
        client_id: Option<String>,

        /// Azure AD tenant ID
        #[arg(long)]
        tenant_id: Option<String>,
    },

    /// Run daemon: continuously fetch, classify, and notify
    Daemon,

    /// One-shot sync: fetch, embed, and classify new emails
    Sync,

    /// Interactive review mode for correcting classifications
    Review,

    /// List recent classifications
    List {
        /// Only show unverified (model-generated) classifications
        #[arg(long)]
        unverified: bool,

        /// Filter by category
        #[arg(long, short)]
        category: Option<String>,

        /// Number of emails to show
        #[arg(long, short, default_value = "20")]
        limit: usize,
    },

    /// Show details for a specific email
    Show {
        /// Email ID
        email_id: String,
    },

    /// Correct a classification
    Correct {
        /// Email ID
        email_id: String,

        /// New category
        #[arg(long, short)]
        category: String,

        /// New importance (0-4)
        #[arg(long, short)]
        importance: i32,
    },

    /// Mark a model classification as verified (user agrees)
    Verify {
        /// Email ID
        email_id: String,
    },

    /// Category management
    Category {
        #[command(subcommand)]
        action: CategoryAction,
    },

    /// Export classifications
    Export {
        /// Output format (csv or json)
        #[arg(long, short, default_value = "csv")]
        format: String,

        /// Output file (stdout if not specified)
        #[arg(long, short)]
        output: Option<String>,
    },

    /// Show statistics
    Stats,
}

#[derive(Subcommand)]
pub enum CategoryAction {
    /// Add a new category
    Add {
        /// Category name
        name: String,

        /// Category description
        #[arg(long, short)]
        description: String,
    },

    /// List all categories
    List,
}
