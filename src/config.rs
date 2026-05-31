use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tracing::TracingConfig;

use crate::server::ServerConfig;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
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

#[derive(Clone, Debug, Serialize, Deserialize)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn env() -> EnvOverride {
        EnvOverride { btcmap_api_key: "key".to_string() }
    }

    #[test]
    fn app_config_default_values() {
        let cfg = AppConfig::default();
        assert_eq!(cfg.btcmap_api_url, "https://api.btcmap.org/rpc");
        assert_eq!(cfg.btcmap_origin, "blink");
    }

    #[test]
    fn config_default() {
        let cfg = Config::default();
        assert_eq!(cfg.app.btcmap_api_url, "https://api.btcmap.org/rpc");
        // Default::default() uses TracingConfig::default(), not default_tracing_config()
        assert_eq!(cfg.tracing.service_name, "dev-rs");
    }

    #[test]
    fn from_path_tracing_default_via_serde() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "{{}}").unwrap();
        let cfg = Config::from_path(f.path(), env()).unwrap();
        // serde uses default_tracing_config() → "btcmap-proxy"
        assert_eq!(cfg.tracing.service_name, "btcmap-proxy");
    }

    #[test]
    fn from_path_minimal_config() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "{{}}").unwrap();
        let cfg = Config::from_path(f.path(), env()).unwrap();
        assert_eq!(cfg.app.btcmap_api_url, "https://api.btcmap.org/rpc");
        assert_eq!(cfg.app.btcmap_origin, "blink");
    }

    #[test]
    fn from_path_full_config() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(
            f,
            "server:\n  port: 8080\napp:\n  btcmap_api_url: http://custom\n  btcmap_origin: myorg"
        )
        .unwrap();
        let cfg = Config::from_path(f.path(), env()).unwrap();
        assert_eq!(cfg.app.btcmap_api_url, "http://custom");
        assert_eq!(cfg.app.btcmap_origin, "myorg");
        assert_eq!(cfg.server.port, 8080);
    }

    #[test]
    fn from_path_missing_file() {
        let err = Config::from_path("/nonexistent/path/config.yml", env()).unwrap_err();
        assert!(err.to_string().contains("Couldn't read config file"));
    }

    #[test]
    fn from_path_invalid_yaml() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, ": this is not: valid: yaml:::").unwrap();
        let err = Config::from_path(f.path(), env()).unwrap_err();
        assert!(err.to_string().contains("Couldn't parse config file"));
    }
}
