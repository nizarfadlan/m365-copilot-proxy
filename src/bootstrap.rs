use std::path::{Path, PathBuf};

use tracing::info;

use crate::config::{AppConfig, ServeOverrides};

const CONFIG_TEMPLATE: &str = include_str!("../config.example.toml");
const ENV_EXAMPLE: &str = include_str!("../.env.example");

#[derive(Debug, Default)]
pub struct BootstrapReport {
    pub config_path: Option<PathBuf>,
    pub config_created: bool,
    pub env_example_created: bool,
}

/// Prepare config/env files on first run so `serve` works out of the box.
pub fn bootstrap(overrides: &ServeOverrides) -> Result<BootstrapReport, String> {
    let mut report = BootstrapReport::default();

    if overrides.config_path.is_some() {
        report.config_path = overrides.config_path.clone();
        return Ok(report);
    }

    if Path::new("config.toml").exists() {
        report.config_path = Some(PathBuf::from("config.toml"));
        return Ok(report);
    }

    if let Ok(env_path) = std::env::var("M365_CONFIG") {
        report.config_path = Some(PathBuf::from(env_path));
        return Ok(report);
    }

    let user_config = dirs::config_dir().map(|d| d.join("m365-copilot-proxy").join("config.toml"));

    if let Some(ref path) = user_config {
        if path.exists() {
            report.config_path = Some(path.clone());
            return Ok(report);
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(path, CONFIG_TEMPLATE).map_err(|e| e.to_string())?;
        report.config_created = true;
        report.config_path = Some(path.clone());
        info!(path = %path.display(), "created default config.toml");
    }

    let env_example = PathBuf::from(".env.example");
    if !env_example.exists() {
        std::fs::write(&env_example, ENV_EXAMPLE).map_err(|e| e.to_string())?;
        report.env_example_created = true;
        info!("created .env.example in current directory");
    }

    Ok(report)
}

pub fn print_welcome(report: &BootstrapReport, config: &AppConfig) {
    if report.config_created {
        println!(
            "\nFirst run: wrote config to {}\nEdit it or use M365_* env vars. See README.\n",
            report
                .config_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        );
    }

    let token_missing = crate::cdp::needs_substrate_token(
        crate::token_store::read_env_token(&config.token.env_file).as_deref(),
    );

    if token_missing && config.token.capture_on_start {
        println!("No valid token yet. The proxy will:");
        println!("  1. Open Microsoft Edge with M365 Copilot");
        println!("  2. Wait for you to sign in");
        println!("  3. Capture the token when you click the message box and type one character");
        println!("\nManual fallback: copilot-openai-proxy set-token\n");
    }
}
