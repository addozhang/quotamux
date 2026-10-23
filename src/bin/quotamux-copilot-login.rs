//! Run GitHub's device flow and print a Copilot API token for the
//! `copilot` adapter. The token does not expire; store it as the
//! credential `api_key` in the routing configuration.

use std::time::{Duration, Instant};

use clap::Parser;
use quotamux::copilot_login::{self, DeviceEndpoints, PollOutcome};

type AnyError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Parser, Debug)]
#[command(about = "Obtain a GitHub Copilot API token via GitHub's device flow")]
struct Args {
    /// Write the token to stdout alone, without usage instructions.
    #[arg(long)]
    quiet: bool,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), AnyError> {
    let args = Args::parse();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let endpoints = DeviceEndpoints::default();

    if !args.quiet {
        println!("Requesting a device code from GitHub…");
    }
    let code = copilot_login::request_device_code(&client, &endpoints).await?;
    println!();
    println!("  1. Open    {}", code.verification_uri);
    println!("  2. Enter   {}", code.user_code);
    println!();
    if !args.quiet {
        println!("Waiting for authorization (Ctrl+C cancels)…");
    }

    let deadline = Instant::now() + Duration::from_secs(code.expires_in.max(1));
    let mut interval = Duration::from_secs(code.interval.max(1));
    let access_token = loop {
        if Instant::now() >= deadline {
            return Err("the device code expired; run the login again".into());
        }
        tokio::time::sleep(interval).await;
        match copilot_login::poll_token(&client, &endpoints, &code.device_code).await? {
            PollOutcome::Pending => {}
            PollOutcome::SlowDown { interval_secs } => {
                interval = Duration::from_secs(interval_secs.saturating_add(5).max(1));
            }
            PollOutcome::Complete { access_token } => break access_token,
            PollOutcome::Expired => {
                return Err("the device code expired; run the login again".into());
            }
            PollOutcome::Denied => return Err("the authorization request was denied".into()),
        }
    };

    if args.quiet {
        println!("{access_token}");
        return Ok(());
    }
    let login = copilot_login::github_login(&client, &endpoints, &access_token)
        .await
        .unwrap_or_else(|| "unknown".into());
    println!("Authorized GitHub account: {login}");
    println!();
    println!("Copilot API token (does not expire; treat it like a password):");
    println!();
    println!("  {access_token}");
    println!();
    println!("Paste it as the api_key of a copilot backend credential:");
    println!();
    println!("  [[backends]]");
    println!("  id = \"copilot\"");
    println!("  adapter = \"copilot\"");
    println!();
    println!("  [[backends.credentials]]");
    println!("  id = \"copilot-token\"");
    println!("  api_key = \"<token>\"");
    println!();
    println!("List the model IDs enabled on your plan with:");
    println!();
    println!("  curl -H \"Authorization: Bearer <token>\" https://api.githubcopilot.com/models");
    Ok(())
}
