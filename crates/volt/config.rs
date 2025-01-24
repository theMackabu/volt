use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf, process};
use toml::Value;
use uuid::Uuid;

const DEFAULT_CONFIG: &str = include_str!("default.toml");

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct VoltConfig {
    pub server: String,
    pub volt_id: String,
    pub settings: Config,

    #[serde(skip)]
    path: PathBuf,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Config {
    pub tls: bool,
    pub cache: Vec<String>,
    pub wrap: String,
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

    pub fn server(&self) -> Result<(String, String)> {
        let (token, rest) = self.server.split_once('@').ok_or_else(|| anyhow::anyhow!("Invalid server format"))?;
        Ok((token.to_string(), rest.to_string()))
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
            println!("😅 Configuration matches default template - please edit it.");
            process::exit(1);
        }

        println!("📝 Loaded Volt Config\n🚀 Volt is ready!");
        current_toml.try_into().map_err(Into::into)
    }
}
