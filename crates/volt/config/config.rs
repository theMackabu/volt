use crate::{colors, helpers::parse_server};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::PathBuf, process};
use toml::Value;
use uuid::Uuid;

const DEFAULT_CONFIG: &str = include_str!("default.toml");

pub type Servers = BTreeMap<String, Server>;

#[derive(PartialEq)]
pub enum Route {
    Push,
    Pull,
    Health,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct VoltConfig {
    pub volt_id: String,
    pub settings: Config,

    #[serde(skip)]
    pub path: PathBuf,

    #[serde(skip)]
    pub servers: Servers,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Config {
    pub server: String,
    pub cache: Vec<String>,
    pub wrap: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Server {
    pub tls: bool,
    pub address: String,
    pub token: Option<String>,
}

impl VoltConfig {
    pub fn new(path: PathBuf) -> Self {
        let mut config = Self::default();
        config.path = path;
        return config;
    }

    pub fn init(&self) -> Result<VoltConfig> {
        if self.path.exists() {
            return self.load();
        }

        let config = DEFAULT_CONFIG.replace("{volt_id}", &Uuid::new_v4().to_string());

        fs::write(&self.path, config)?;
        println!("{} Created a new config - please fill it out.", crate::colors::BOLT);

        process::exit(0);
    }

    pub fn get_server(&self, route: Route) -> Result<(String, String)> {
        let server = self.servers.get(&self.settings.server).ok_or_else(|| {
            let name = &self.settings.server;
            anyhow!("server '{name}' does not exist")
        })?;

        let route = match route {
            Route::Push => "push",
            Route::Pull => "pull",
            Route::Health => "health",
        };

        let tls = if server.tls { "https" } else { "http" };
        let url = format!("{tls}://{}/{route}/{}", server.address, self.volt_id);
        let header = server.token.as_ref().map_or_else(|| String::new(), |t| format!("Bearer {}", t));

        Ok((url, header))
    }

    pub fn get_servers(&self) -> Result<PathBuf> {
        match home::home_dir() {
            Some(mut path) => {
                path.push(".volt");
                path.push("servers");

                if !path.exists() {
                    fs::create_dir_all(&path)?;
                }

                return Ok(path);
            }
            None => {
                eprintln!("{} Impossible to get your home directory", colors::FAIL);
                process::exit(0);
            }
        }
    }

    pub fn load_servers(&mut self) -> Result<()> {
        let path = self.get_servers()?;
        let mut servers = BTreeMap::new();

        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            let file_name = path
                .file_stem()
                .and_then(|os_str| os_str.to_str())
                .ok_or_else(|| anyhow!("Invalid filename for path {:?}", path))?
                .to_string();

            let content = fs::read_to_string(&path).with_context(|| format!("Failed to read file {:?}", path))?;
            let line = content.trim();
            let server = parse_server(line).with_context(|| format!("Failed to parse server from file {:?}", path))?;

            servers.insert(file_name, server);
        }

        Ok(self.servers = servers)
    }

    fn load(&self) -> Result<VoltConfig> {
        let content = fs::read_to_string(&self.path)?;
        let default_toml: Value = toml::from_str(DEFAULT_CONFIG)?;
        let current_toml: Value = toml::from_str(&content)?;

        let filter_volt_id = |v: &Value| {
            let mut cloned = v.clone();
            cloned.as_table_mut().and_then(|t| t.remove("volt_id"));
            cloned
        };

        if filter_volt_id(&default_toml) == filter_volt_id(&current_toml) {
            eprintln!("😅 Configuration matches default template - please edit it.");
            process::exit(1);
        }

        println!("📝 Loaded Volt Config\n🚀 Volt is ready!");
        current_toml.try_into().map_err(Into::into)
    }
}
