mod colors;
mod hash;
mod helpers;

#[path = "config/config.rs"]
mod config;

use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
use colored::Colorize;
use config::{Route, VoltConfig};
use indicatif::{ProgressBar, ProgressStyle};
use inquire::{Confirm, CustomType, Password, PasswordDisplayMode, Text, validator::Validation};
use reqwest::{Client, StatusCode};

use std::{
    fs,
    path::PathBuf,
    process::{self, Command, ExitCode},
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
    #[command(visible_alias = "get", visible_alias = "P")]
    Push,
    /// Pull cache from server
    #[command(visible_alias = "set", visible_alias = "p")]
    Pull,
    /// Run build with caching
    #[command(visible_alias = "start", visible_alias = "r")]
    Run,
    /// Server management
    #[command(visible_alias = "srv", visible_alias = "s")]
    Server {
        #[command(subcommand)]
        command: Option<Server>,
    },
}

#[derive(Subcommand)]
enum Server {
    /// Add a new server
    #[command(visible_alias = "add", visible_alias = "n")]
    New,
    /// Remove an existing server
    #[command(visible_alias = "delete", visible_alias = "rm")]
    Remove {
        /// Name of the server to remove
        name: String,
    },
    /// List all configured servers
    #[command(visible_alias = "ls", visible_alias = "l")]
    List,
    /// Test connection to the server
    #[command(visible_alias = "status", visible_alias = "t")]
    Test,
    #[command(visible_alias = "i")]
    /// Display detailed information about a server
    Info {
        /// Name of the server to inspect
        name: String,
    },
}

#[tokio::main]
async fn main() -> Result<ExitCode> {
    let cli = Cli::parse();

    let mut config = VoltConfig::new(cli.path).init()?;
    let client = helpers::create_client(&mut config)?;
    let mut services = Services::new(config, client);

    match cli.command.unwrap_or(Commands::Run) {
        Commands::Push => services.push_cache().await?,
        Commands::Pull => services.pull_cache().await?,
        Commands::Run => services.run_build().await?,
        Commands::Server { command } => match command.unwrap_or(Server::New) {
            Server::New => services.server_add().await?,
            Server::List => services.server_list().await?,
            Server::Test => services.server_test().await?,
            Server::Remove { name } => services.server_remove(&name).await?,
            Server::Info { name } => services.server_info(&name).await?,
        },
    };

    Ok(ExitCode::SUCCESS)
}

impl Services {
    pub fn new(config: VoltConfig, client: Client) -> Self { Self { config, client } }

    pub async fn pull_cache(&self) -> Result<ExitCode> {
        let start = Instant::now();
        let (url, header) = self.config.get_server(Route::Pull)?;

        let hash_dirs = self.config.settings.hash.as_ref().unwrap_or(&self.config.settings.cache);
        let hash = hash::compute_cache(hash_dirs)?;

        println!("{hash} {hash_dirs:?}");

        let pb = ProgressBar::new_spinner();
        let style = ProgressStyle::with_template("\n{spinner:.green} {msg}")
            .unwrap()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", "✓"]);

        pb.set_style(style);
        pb.enable_steady_tick(std::time::Duration::from_millis(80));

        let response = match self.client.get(&url).header("Authorization", header).header("X-Volt-Hash", hash).send().await {
            Ok(next) => next,
            Err(_) => {
                pb.finish_and_clear();
                return Err(anyhow!("unable to connect, is the server up?"));
            }
        };

        if response.status() == StatusCode::NOT_MODIFIED {
            pb.finish_with_message("Cache is up to date");
            return Ok(ExitCode::SUCCESS);
        }

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
        let start = Instant::now();
        let (url, header) = self.config.get_server(Route::Push)?;

        let hash_dirs = self.config.settings.hash.as_ref().unwrap_or(&self.config.settings.cache);
        let hash = hash::compute_cache(hash_dirs)?;

        println!("{hash} {hash_dirs:?}");

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
        let length = helpers::format_size(compressed.len());

        let response = match self.client.post(&url).header("Authorization", header).header("X-Volt-Hash", hash).body(compressed).send().await {
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

        pb.finish_with_message(format!("Cached {} in {}", length.bright_cyan(), format!("{:.2?}", start.elapsed()).green()));
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

    async fn server_add(&self) -> Result<ExitCode> {
        let servers_dir = self.config.get_servers()?;
        let servers_dir_owned = servers_dir.to_owned();

        println!(
            "\nWelcome to {} {}, {}!\n",
            " Volt ".on_bright_green().black(),
            format!("v{}", env!("CARGO_PKG_VERSION")).bright_green(),
            whoami::username(),
        );

        let name = Text::new("What should we name your new server?")
            .with_validator(move |input: &str| {
                let input = input.trim();
                if input.is_empty() {
                    Ok(Validation::Invalid("Name cannot be empty".into()))
                } else if servers_dir_owned.join(input).exists() {
                    Ok(Validation::Invalid("Server already exists".into()))
                } else if input.contains('/') || input.contains('\\') {
                    Ok(Validation::Invalid("Invalid characters in name".into()))
                } else {
                    Ok(Validation::Valid)
                }
            })
            .with_help_message("Unique identifier for this server")
            .prompt()?;

        let address = Text::new("What's the server address?")
            .with_help_message("Domain or IP address (e.g. volt.build or 192.168.1.1)")
            .with_validator(|input: &str| {
                if input.trim().is_empty() {
                    Ok(Validation::Invalid("Address cannot be empty".into()))
                } else {
                    Ok(Validation::Valid)
                }
            })
            .prompt()?
            .trim()
            .to_string();

        let port = CustomType::<u16>::new("What port is the server using?")
            .with_help_message("Typically 443 for TLS, 80 for plain TCP")
            .with_error_message("Please enter a valid port (1-65535)")
            .with_default(443)
            .prompt()?;

        let tls = Confirm::new("Are you using TLS/SSL?")
            .with_default(true)
            .with_help_message("Required for secure connections")
            .prompt()?;

        let mut token = None;
        if Confirm::new("Would you like to add an authentication token?")
            .with_default(false)
            .with_help_message("Required if the server needs authentication")
            .prompt()?
        {
            token = Some(
                Password::new("Enter your authentication token:")
                    .without_confirmation()
                    .with_display_toggle_enabled()
                    .with_formatter(&|_| String::from("✓"))
                    .with_display_mode(PasswordDisplayMode::Masked)
                    .with_help_message("This will be stored in plain text")
                    .with_validator(|input: &str| {
                        if input.trim().is_empty() {
                            Ok(Validation::Invalid("Token cannot be empty".into()))
                        } else {
                            Ok(Validation::Valid)
                        }
                    })
                    .prompt()?
                    .trim()
                    .to_string(),
            );
        }

        let protocol = if tls { "tls://" } else { "" };
        let auth_part = token.as_ref().map_or(String::new(), |t| format!("{}@", t));
        let url = format!("{}{}{}:{}", protocol, auth_part, address, port);

        helpers::parse_server(&url).context("Invalid server configuration")?;

        let server_path = servers_dir.join(&name);
        fs::write(server_path, &url)?;

        let redacted_url = {
            let url_str = url.as_str();

            if url_str.contains('@') {
                let (protocol, rest) = if url_str.starts_with("tls://") { ("tls://", &url_str["tls://".len()..]) } else { ("", url_str) };

                if let Some(at_pos) = rest.find('@') {
                    let (token_part, host_port) = rest.split_at(at_pos);
                    let host_port = &host_port[1..];
                    let redacted_token = "*".repeat(token_part.len());
                    format!("{}{}@{}", protocol, redacted_token, host_port)
                } else {
                    url_str.to_string()
                }
            } else {
                url_str.to_string()
            }
        };

        println!("\n{} Successfully configured server {}: {}", colors::OK, name.bright_cyan(), redacted_url.bright_blue());

        Ok(ExitCode::SUCCESS)
    }

    async fn server_remove(&self, name: &str) -> Result<ExitCode> {
        let servers_dir = self.config.get_servers()?;
        let server_path = servers_dir.join(name);

        if !server_path.exists() {
            eprintln!("\n{} Server '{name}' not found", colors::WARN);
            return Ok(ExitCode::FAILURE);
        }

        fs::remove_file(server_path)?;
        println!("\n{} Server '{name}' removed", colors::OK);

        Ok(ExitCode::SUCCESS)
    }

    async fn server_list(&mut self) -> Result<ExitCode> {
        self.config.load_servers()?;
        let servers = &self.config.servers;

        if servers.is_empty() {
            eprintln!("\n{} No servers configured", colors::WARN);
            return Ok(ExitCode::FAILURE);
        }

        println!("\nConfigured servers:");
        for (name, server) in servers {
            let token_status = if server.token.is_some() { "🔑" } else { "ó﹏ò｡" };
            println!("  {} - {}{} ({})", name.bright_cyan(), if server.tls { "🔒 " } else { "" }, server.address, token_status);
        }

        Ok(ExitCode::SUCCESS)
    }

    async fn server_info(&mut self, name: &str) -> Result<ExitCode> {
        let servers_dir = self.config.get_servers()?;
        let server_path = servers_dir.join(name);

        let content = fs::read_to_string(&server_path).unwrap_or_else(|_| {
            eprintln!("\n{} Server '{name}' not found", colors::FAIL);
            process::exit(1)
        });

        let server = helpers::parse_server(&content)?;

        println!("\nServer information for {}", name.bright_magenta());
        println!("  Address: {}", server.address.bright_cyan());
        println!("  TLS: {}", if server.tls { "Enabled".green() } else { "Disabled".yellow() });
        println!("  Authentication: {}", if server.token.is_some() { "Token configured".green() } else { "No token".red() });

        self.config.settings.server = name.to_string();
        self.server_test().await?;

        Ok(ExitCode::SUCCESS)
    }

    async fn server_test(&self) -> Result<ExitCode> {
        let name = &self.config.settings.server;

        let (url, header) = self.config.get_server(Route::Health).unwrap_or_else(|_| {
            eprintln!("\n{} Server '{name}' not found", colors::FAIL);
            process::exit(1)
        });

        let response = self.client.get(&url).header("Authorization", header).send().await.context("Connection failed")?;

        if response.status().is_success() {
            println!("\n{} Successfully connected to {name}", colors::OK);
        } else {
            println!("\n{} Connection failed: {}", colors::FAIL, response.status());
        }

        Ok(ExitCode::SUCCESS)
    }
}
