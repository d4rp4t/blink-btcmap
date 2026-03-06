use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tracing::TracingConfig;

use crate::server::ServerConfig;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub app: AppConfig,
    #[serde(default = "default_tracing_config")]
    pub tracing: TracingConfig,
}

fn default_tracing_config() -> TracingConfig {
    TracingConfig {
        service_name: "btcmap-proxy".to_string(),
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default = "default_btcmap_api_url")]
    pub btcmap_api_url: String,
    #[serde(default = "default_btcmap_origin")]
    pub btcmap_origin: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            btcmap_api_url: default_btcmap_api_url(),
            btcmap_origin: default_btcmap_origin(),
        }
    }
}

fn default_btcmap_api_url() -> String {
    "https://api.btcmap.org/rpc".to_string()
}

fn default_btcmap_origin() -> String {
    "blink".to_string()
}

pub struct EnvOverride {
    pub btcmap_api_key: String,
}

impl Config {
    pub fn from_path(
        path: impl AsRef<Path>,
        EnvOverride { btcmap_api_key: _ }: EnvOverride,
    ) -> anyhow::Result<Self> {
        let config_file = std::fs::read_to_string(&path)
            .context(format!("Couldn't read config file: {}", path.as_ref().display()))?;
        let config: Config =
            serde_yaml::from_str(&config_file).context("Couldn't parse config file")?;
        Ok(config)
    }
}
