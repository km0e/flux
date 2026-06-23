use crate::rpc::ProviderParams;
use anyhow::Context;
use flux_mcp::McpServerConfig;
use serde::Deserialize;
use std::path::Path;
use tracing::{info, warn};

/// Global server configuration loaded from a TOML file.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ServerConfig {
    pub server: ServerListenConfig,
    pub provider: Option<ProviderParams>,
    pub workdir: Option<String>,
    #[serde(rename = "mcp_servers")]
    pub mcp_servers: Vec<McpServerConfig>,
    /// Optional system prompt / instructions for the agent.
    pub preamble: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerListenConfig {
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

impl Default for ServerListenConfig {
    fn default() -> Self {
        Self {
            mode: default_mode(),
            port: default_port(),
        }
    }
}

fn default_mode() -> String {
    "stdio".to_string()
}

fn default_port() -> u16 {
    8080
}

impl ServerConfig {
    /// Load configuration from a TOML file.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;
        let config: Self = toml::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {}", path.display()))?;
        Ok(config)
    }

    /// Load configuration if the file exists, otherwise return defaults.
    pub fn load_or_default(path: &Path) -> Self {
        if path.exists() {
            match Self::load(path) {
                Ok(config) => {
                    info!(config_file = %path.display(), "Loaded server configuration");
                    config
                }
                Err(e) => {
                    warn!(config_file = %path.display(), error = ?e, "Failed to load config; using defaults");
                    Self::default()
                }
            }
        } else {
            info!(config_file = %path.display(), "No config file found; using defaults");
            Self::default()
        }
    }
}
