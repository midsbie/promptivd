use std::io::{self, Read};
use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use reqwest::Client;
use serde_json::json;

use promptivd::models::{InsertTextRequest, Placement, SessionDirective, SourceInfo, TargetSpec};

#[derive(Debug, Copy, Clone, ValueEnum)]
enum SessionDirectiveArg {
    #[value(name = "reuse_or_create")]
    ReuseOrCreate,
    #[value(name = "reuse_only")]
    ReuseOnly,
    #[value(name = "start_fresh")]
    StartFresh,
}

impl From<SessionDirectiveArg> for SessionDirective {
    fn from(value: SessionDirectiveArg) -> Self {
        match value {
            SessionDirectiveArg::ReuseOrCreate => SessionDirective::ReuseOrCreate,
            SessionDirectiveArg::ReuseOnly => SessionDirective::ReuseOnly,
            SessionDirectiveArg::StartFresh => SessionDirective::StartFresh,
        }
    }
}

#[derive(Debug, Copy, Clone, ValueEnum)]
enum PlacementArg {
    #[value(name = "top")]
    Top,
    #[value(name = "bottom")]
    Bottom,
    #[value(name = "cursor")]
    Cursor,
}

impl From<PlacementArg> for Placement {
    fn from(value: PlacementArg) -> Self {
        match value {
            PlacementArg::Top => Placement::Top,
            PlacementArg::Bottom => Placement::Bottom,
            PlacementArg::Cursor => Placement::Cursor,
        }
    }
}

#[derive(Parser)]
#[command(name = "promptivc")]
#[command(about = "CLI client for promptivd daemon")]
#[command(version)]
struct Cli {
    /// Server URL
    #[arg(long, default_value = "http://127.0.0.1:8787")]
    server: String,

    /// Source file path
    #[arg(short = 'f', long)]
    path: Option<PathBuf>,

    /// Client label
    #[arg(short, long, default_value = "CLI")]
    label: String,

    /// Read from stdin instead of arguments
    #[arg(long)]
    stdin: bool,

    /// Text content (if not reading from stdin)
    #[arg(value_name = "TEXT")]
    content: Option<String>,

    /// Target provider
    #[arg(long = "provider", value_name = "PROVIDER")]
    target_provider: Option<String>,

    /// Session directive
    #[arg(long = "session-directive", value_enum, value_name = "DIRECTIVE")]
    session_directive: Option<SessionDirectiveArg>,

    /// Placement preference
    #[arg(long = "placement", value_enum, value_name = "PLACEMENT")]
    placement: Option<PlacementArg>,

    /// Show verbose output
    #[arg(short, long)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // Initialize logging if verbose
    if cli.verbose {
        tracing_subscriber::fmt::init();
    }

    // Get content from stdin or arguments
    let content = if cli.stdin || cli.content.is_none() {
        read_from_stdin()?
    } else {
        cli.content.unwrap()
    };

    if content.trim().is_empty() {
        eprintln!("Error: No content provided");
        std::process::exit(1);
    }

    // Build optional target specification if provider metadata is supplied
    let target = if cli.target_provider.is_some() || cli.session_directive.is_some() {
        Some(TargetSpec {
            provider: cli.target_provider.clone(),
            session_directive: cli.session_directive.map(Into::into),
        })
    } else {
        None
    };

    // Create the request
    let request = InsertTextRequest {
        schema_version: "1.0".to_string(),
        source: SourceInfo {
            client: "cli".to_string(),
            label: Some(cli.label),
            path: cli.path.as_ref().map(|p| p.to_string_lossy().to_string()),
        },
        text: add_snippet_template(&content, cli.path.as_ref()),
        placement: cli.placement.map(Into::into),
        target,
        metadata: json!({
            "cli_version": env!("CARGO_PKG_VERSION"),
            "timestamp": chrono::Utc::now().to_rfc3339()
        }),
    };

    // Create HTTP client
    let client = Client::new();
    let request_builder = client
        .post(format!("{}/v1/insert", cli.server))
        .json(&request);

    if cli.verbose {
        println!("Sending request to: {}/v1/insert", cli.server);
    }

    let response = request_builder.send().await?;
    let status = response.status();
    let body: serde_json::Value = response.json().await?;

    let job_id = body
        .get("job_id")
        .and_then(|v| v.as_str())
        .unwrap_or("<unknown>");

    if !status.is_success() {
        let error_message = body
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("Request failed");
        eprintln!("Job {} failed (status {})", job_id, status);
        eprintln!("Error: {}", error_message);
        std::process::exit(1);
    }

    let result_status = body.get("status").and_then(|v| v.as_str()).unwrap_or("ok");

    if cli.verbose {
        println!("Job {} completed with status {}", job_id, result_status);
        if let Some(chars) = body.get("inserted_chars").and_then(|v| v.as_u64()) {
            println!("Inserted characters: {}", chars);
        }
    } else {
        println!("Job {}: {}", job_id, result_status);
    }

    Ok(())
}

fn read_from_stdin() -> Result<String, io::Error> {
    let mut buffer = String::new();
    io::stdin().read_to_string(&mut buffer)?;
    Ok(buffer)
}

fn add_snippet_template(content: &str, path: Option<&PathBuf>) -> String {
    let path_str = path
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "<stdin>".to_string());

    format!("Snippet from {}:\n{}\n---\n", path_str, content.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_snippet_template() {
        let content = "Hello world";
        let path = Some(PathBuf::from("/test/file.txt"));

        let result = add_snippet_template(content, path.as_ref());
        assert!(result.contains("Snippet from /test/file.txt:"));
        assert!(result.contains("Hello world"));
        assert!(result.ends_with("---\n"));
    }

    #[test]
    fn test_add_snippet_template_no_path() {
        let content = "Hello world";
        let result = add_snippet_template(content, None);
        assert!(result.contains("Snippet from <stdin>:"));
    }
}
