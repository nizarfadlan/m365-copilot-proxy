use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Runtime token/model settings passed to the Copilot client.
#[derive(Debug, Clone)]
pub struct Settings {
    pub access_token: String,
    pub time_zone: String,
    pub model_alias: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            access_token: String::new(),
            time_zone: "Asia/Tokyo".into(),
            model_alias: "m365-copilot".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub token: TokenConfig,
    pub edge: EdgeConfig,
    pub logging: LoggingConfig,
    pub ui: UiConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TokenConfig {
    pub access_token: Option<String>,
    pub env_file: PathBuf,
    pub time_zone: String,
    pub model_alias: String,
    pub auto_refresh: bool,
    pub capture_on_start: bool,
    pub capture_timeout_seconds: u64,
    pub refresh_before_seconds: i64,
    pub refresh_retry_seconds: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct EdgeConfig {
    pub cdp_port: u16,
    pub launch_on_start: bool,
    pub profile_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LoggingConfig {
    pub level: String,
    pub format: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub tui: bool,
    pub tray: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8000,
        }
    }
}

impl Default for TokenConfig {
    fn default() -> Self {
        Self {
            access_token: None,
            env_file: PathBuf::from(".env"),
            time_zone: "Asia/Tokyo".into(),
            model_alias: "m365-copilot".into(),
            auto_refresh: true,
            capture_on_start: true,
            capture_timeout_seconds: 180,
            refresh_before_seconds: 300,
            refresh_retry_seconds: 60,
        }
    }
}

impl Default for EdgeConfig {
    fn default() -> Self {
        Self {
            cdp_port: 9222,
            launch_on_start: true,
            profile_dir: None,
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".into(),
            format: "pretty".into(),
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            tui: true,
            tray: true,
        }
    }
}

/// CLI flags that override config file values when explicitly set.
#[derive(Debug, Clone, Default)]
pub struct ServeOverrides {
    pub config_path: Option<PathBuf>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub cdp_port: Option<u16>,
    pub no_auto_refresh: bool,
    pub no_launch_edge: bool,
    pub no_capture_on_start: bool,
    pub capture_timeout_seconds: Option<u64>,
    pub refresh_before_seconds: Option<i64>,
    pub refresh_retry_seconds: Option<u64>,
    pub no_tui: bool,
    pub no_tray: bool,
    pub log_level: Option<String>,
}

impl AppConfig {
    pub fn load(overrides: &ServeOverrides) -> Self {
        dotenvy::dotenv().ok();

        let mut config = Self::default();
        if let Some(path) = discover_config_path(overrides.config_path.as_deref()) {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(file_config) = toml::from_str::<AppConfig>(&text) {
                    config = file_config;
                } else if let Ok(parsed) = toml::from_str::<toml::Value>(&text) {
                    config = merge_partial_config(config, parsed);
                }
            }
        }

        apply_env_overrides(&mut config);
        apply_cli_overrides(&mut config, overrides);
        config
    }

    pub fn settings(&self) -> Settings {
        let access_token = self
            .token
            .access_token
            .clone()
            .filter(|t| !t.is_empty())
            .or_else(|| crate::token_store::read_env_token(&self.token.env_file))
            .unwrap_or_default();

        Settings {
            access_token,
            time_zone: self.token.time_zone.clone(),
            model_alias: self.token.model_alias.clone(),
        }
    }

    pub fn listen_addr(&self) -> String {
        format!("{}:{}", self.server.host, self.server.port)
    }

    pub fn edge_profile_dir(&self) -> PathBuf {
        self.edge.profile_dir.clone().unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".m365-copilot-proxy")
                .join("edge-profile")
        })
    }
}

fn discover_config_path(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(path.to_path_buf());
    }
    if let Ok(path) = std::env::var("M365_CONFIG") {
        return Some(PathBuf::from(path));
    }
    let local = PathBuf::from("config.toml");
    if local.exists() {
        return Some(local);
    }
    dirs::config_dir().map(|d| d.join("m365-copilot-proxy").join("config.toml"))
}

fn merge_partial_config(mut base: AppConfig, value: toml::Value) -> AppConfig {
    if let Some(table) = value.get("server").and_then(|v| v.clone().try_into().ok()) {
        base.server = table;
    }
    if let Some(table) = value.get("token").and_then(|v| v.clone().try_into().ok()) {
        base.token = table;
    }
    if let Some(table) = value.get("edge").and_then(|v| v.clone().try_into().ok()) {
        base.edge = table;
    }
    if let Some(table) = value.get("logging").and_then(|v| v.clone().try_into().ok()) {
        base.logging = table;
    }
    if let Some(table) = value.get("ui").and_then(|v| v.clone().try_into().ok()) {
        base.ui = table;
    }
    base
}

fn apply_env_overrides(config: &mut AppConfig) {
    if let Ok(v) = std::env::var("M365_HOST") {
        config.server.host = v;
    }
    if let Ok(v) = std::env::var("M365_PORT") {
        if let Ok(port) = v.parse() {
            config.server.port = port;
        }
    }
    if let Ok(v) = std::env::var("M365_ACCESS_TOKEN") {
        config.token.access_token = Some(v);
    }
    if let Ok(v) = std::env::var("M365_ENV_FILE") {
        config.token.env_file = PathBuf::from(v);
    }
    if let Ok(v) = std::env::var("M365_TIME_ZONE") {
        config.token.time_zone = v;
    }
    if let Ok(v) = std::env::var("M365_MODEL_ALIAS") {
        config.token.model_alias = v;
    }
    if let Ok(v) = std::env::var("M365_CDP_PORT") {
        if let Ok(port) = v.parse() {
            config.edge.cdp_port = port;
        }
    }
    if let Ok(v) = std::env::var("M365_AUTO_REFRESH") {
        config.token.auto_refresh = parse_bool(&v);
    }
    if let Ok(v) = std::env::var("M365_CAPTURE_ON_START") {
        config.token.capture_on_start = parse_bool(&v);
    }
    if let Ok(v) = std::env::var("M365_CAPTURE_TIMEOUT_SECONDS") {
        if let Ok(n) = v.parse() {
            config.token.capture_timeout_seconds = n;
        }
    }
    if let Ok(v) = std::env::var("M365_REFRESH_BEFORE_SECONDS") {
        if let Ok(n) = v.parse() {
            config.token.refresh_before_seconds = n;
        }
    }
    if let Ok(v) = std::env::var("M365_REFRESH_RETRY_SECONDS") {
        if let Ok(n) = v.parse() {
            config.token.refresh_retry_seconds = n;
        }
    }
    if let Ok(v) = std::env::var("M365_LAUNCH_EDGE") {
        config.edge.launch_on_start = parse_bool(&v);
    }
    if let Ok(v) = std::env::var("M365_EDGE_PROFILE_DIR") {
        config.edge.profile_dir = Some(PathBuf::from(v));
    }
    if let Ok(v) = std::env::var("M365_LOG_LEVEL") {
        config.logging.level = v;
    }
    if let Ok(v) = std::env::var("M365_LOG_FORMAT") {
        config.logging.format = v;
    }
    if let Ok(v) = std::env::var("M365_TUI") {
        config.ui.tui = parse_bool(&v);
    }
    if let Ok(v) = std::env::var("M365_TRAY") {
        config.ui.tray = parse_bool(&v);
    }
}

fn apply_cli_overrides(config: &mut AppConfig, overrides: &ServeOverrides) {
    if let Some(ref host) = overrides.host {
        config.server.host = host.clone();
    }
    if let Some(port) = overrides.port {
        config.server.port = port;
    }
    if let Some(port) = overrides.cdp_port {
        config.edge.cdp_port = port;
    }
    if overrides.no_auto_refresh {
        config.token.auto_refresh = false;
    }
    if overrides.no_launch_edge {
        config.edge.launch_on_start = false;
    }
    if overrides.no_capture_on_start {
        config.token.capture_on_start = false;
    }
    if let Some(v) = overrides.capture_timeout_seconds {
        config.token.capture_timeout_seconds = v;
    }
    if let Some(v) = overrides.refresh_before_seconds {
        config.token.refresh_before_seconds = v;
    }
    if let Some(v) = overrides.refresh_retry_seconds {
        config.token.refresh_retry_seconds = v;
    }
    if overrides.no_tui {
        config.ui.tui = false;
    }
    if overrides.no_tray {
        config.ui.tray = false;
    }
    if let Some(ref level) = overrides.log_level {
        config.logging.level = level.clone();
    }
}

fn parse_bool(value: &str) -> bool {
    matches!(
        value.trim().to_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_overrides_host_and_port() {
        std::env::set_var("M365_HOST", "0.0.0.0");
        std::env::set_var("M365_PORT", "9000");
        let mut config = AppConfig::default();
        apply_env_overrides(&mut config);
        assert_eq!(config.server.host, "0.0.0.0");
        assert_eq!(config.server.port, 9000);
        std::env::remove_var("M365_HOST");
        std::env::remove_var("M365_PORT");
    }

    #[test]
    fn default_config_matches_upstream() {
        let config = AppConfig::default();
        assert_eq!(config.server.port, 8000);
        assert_eq!(config.token.model_alias, "m365-copilot");
        assert_eq!(config.token.time_zone, "Asia/Tokyo");
    }
}
