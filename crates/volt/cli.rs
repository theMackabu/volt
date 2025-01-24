mod colors;
mod config;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use config::VoltConfig;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::Client;

use std::{
    path::PathBuf,
    process::{Command, ExitCode},
    time::{Duration, Instant},
};

struct Services {
    pub config: VoltConfig,
    pub client: Client,
}

#[derive(Parser)]
#[command(name = "volt", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    /// Path to load config
    #[arg(short, long, default_value = "volt.toml")]
    path: PathBuf,
}

#[derive(Subcommand)]
enum Commands {
    /// Push cache to server
    Push,
    /// Pull cache from server
    Pull,
    /// Run build with caching
    Run,
}

mod app {
    use super::{Client, Result, VoltConfig};

    pub fn create_client(config: &VoltConfig) -> Result<Client> {
        config.server()?;
        Ok(Client::builder().build()?)
    }

    pub fn format_size(bytes: usize) -> String {
        const UNITS: [&str; 4] = ["b", "kb", "mb", "gb"];
        let mut size = bytes as f64;
        let mut unit_index = 0;

        while size >= 1024.0 && unit_index < UNITS.len() - 1 {
            size /= 1024.0;
            unit_index += 1;
        }

        match unit_index {
            0 => format!("{:.0}{}", size, UNITS[unit_index]),
            _ => format!("{:.1}{}", size, UNITS[unit_index]),
        }
    }
}

#[tokio::main]
async fn main() -> Result<ExitCode> {
    let cli = Cli::parse();

    let config = VoltConfig::new(cli.path).init()?;
    let client = app::create_client(&config)?;
    let services = Services::new(config, client);

    match cli.command.unwrap_or(Commands::Run) {
        Commands::Push => services.push_cache().await?,
        Commands::Pull => services.pull_cache().await?,
        Commands::Run => services.run_build().await?,
    };

    Ok(ExitCode::SUCCESS)
}

impl Services {
    pub fn new(config: VoltConfig, client: Client) -> Self { Self { config, client } }

    pub async fn pull_cache(&self) -> Result<ExitCode> {
        let start = Instant::now();
        let (token, server) = self.config.server()?;
        let url = format!("{}://{}/pull/{}", if self.config.settings.tls { "https" } else { "http" }, server, self.config.volt_id);

        let pb = ProgressBar::new_spinner();
        let style = ProgressStyle::with_template("\n{spinner:.green} {msg}")
            .unwrap()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", "✓"]);

        pb.set_style(style);
        pb.enable_steady_tick(std::time::Duration::from_millis(80));

        let response = match self.client.get(&url).header("Authorization", format!("Bearer {}", token)).send().await {
            Ok(next) => next,
            Err(_) => {
                pb.finish_and_clear();
                return Err(anyhow!("unable to connect, is the server up?"));
            }
        };

        if !response.status().is_success() {
            pb.finish_and_clear();
            return Err(anyhow!(response.status()));
        }

        pb.set_message("Downloading archive...");

        let compressed = response.bytes().await?;
        let decoder = zstd::stream::decode_all(&*compressed)?;

        pb.set_message("Extracting...");

        for dir in &self.config.settings.cache {
            if std::path::Path::new(dir).exists() {
                tokio::fs::remove_dir_all(dir).await?;
            }
        }

        let mut archive = tar::Archive::new(&*decoder);
        archive.unpack(".")?;

        pb.finish_with_message(format!("Cache restored in {}", format!("{:.2?}", start.elapsed()).green()));
        Ok(ExitCode::SUCCESS)
    }

    pub async fn push_cache(&self) -> Result<ExitCode> {
        let (token, server) = self.config.server()?;
        let url = format!("{}://{}/push/{}", if self.config.settings.tls { "https" } else { "http" }, server, self.config.volt_id);

        let pb = ProgressBar::new_spinner();
        let style = ProgressStyle::with_template("\n{spinner:.green} {msg}")
            .unwrap()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", "✓"]);

        pb.set_style(style);
        pb.enable_steady_tick(Duration::from_millis(80));
        pb.set_message("Creating archive...");

        let mut buffer = Vec::new();
        {
            let mut ar = tar::Builder::new(&mut buffer);
            for dir in &self.config.settings.cache {
                ar.append_dir_all(dir, dir)?;
            }
            ar.finish()?;
        }

        pb.set_message("Compressing...");

        let mut encoder = zstd::stream::Encoder::new(Vec::new(), 3)?;
        {
            encoder.multithread(4)?;
            std::io::copy(&mut &buffer[..], &mut encoder)?;
        }

        let compressed = encoder.finish()?;
        let length = app::format_size(compressed.len());

        let response = match self.client.post(&url).header("Authorization", format!("Bearer {}", token)).body(compressed).send().await {
            Ok(next) => next,
            Err(_) => {
                pb.finish_and_clear();
                return Err(anyhow!("unable to connect, is the server up?"));
            }
        };

        pb.set_message("Uploading...");

        if !response.status().is_success() {
            pb.finish_and_clear();
            return Err(anyhow!(response.status()));
        }

        pb.finish_with_message(format!("Cached {} of files", length.bright_cyan()));
        Ok(ExitCode::SUCCESS)
    }

    pub async fn run_build(&self) -> Result<ExitCode> {
        let start = Instant::now();
        let name = self.config.settings.wrap.split_whitespace().next().unwrap_or_default();

        println!("🔥 Starting {}", self.config.settings.wrap);

        if let Err(err) = self.pull_cache().await {
            eprintln!("\n{} Cache pull failed: {err}", colors::FAIL);
        }

        let status = Command::new("sh")
            .arg("-c")
            .arg(&self.config.settings.wrap)
            .status()
            .with_context(|| format!("{} Failed to execute {name}", colors::FAIL))?;

        let code = status.code().unwrap_or_default();

        if !status.success() {
            eprintln!("{} Failed with exit code {code} in {}", colors::FAIL, format!("{:.2?}", start.elapsed()).yellow());
            return Ok(ExitCode::FAILURE);
        }

        if let Err(err) = self.push_cache().await {
            eprintln!("\n{} Cache push failed: {err}", colors::FAIL);
        }

        println!("{} Finished successfully in {}", colors::OK, format!("{:.2?}", start.elapsed()).yellow());
        Ok(ExitCode::SUCCESS)
    }
}
