use std::path::PathBuf;
use std::process;

use clap::{Parser, Subcommand};
use tracing::error;

use m365_copilot_proxy::bootstrap::bootstrap;
use m365_copilot_proxy::cdp::launch_debug_edge_on_port;
use m365_copilot_proxy::config::{AppConfig, ServeOverrides};
use m365_copilot_proxy::doctor::run_doctor;
use m365_copilot_proxy::runtime::{capture_token_interactive, run_serve, set_token_interactive};

#[derive(Parser)]
#[command(name = "copilot-openai-proxy")]
#[command(version)]
#[command(about = "OpenAI-compatible shim for Microsoft 365 Copilot Chat (Rust port)")]
#[command(
    long_about = "Rust port of https://github.com/kuchris/m365-copilot-openai-proxy\n\
                  Configure via config.toml and/or environment variables (see README)."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the HTTP proxy server with TUI dashboard
    Serve {
        #[arg(long, help = "Path to config.toml")]
        config: Option<PathBuf>,
        #[arg(long, env = "M365_HOST")]
        host: Option<String>,
        #[arg(long, env = "M365_PORT")]
        port: Option<u16>,
        #[arg(long, env = "M365_CDP_PORT")]
        cdp_port: Option<u16>,
        #[arg(long)]
        no_auto_refresh: bool,
        #[arg(long)]
        no_launch_edge: bool,
        #[arg(long)]
        no_capture_on_start: bool,
        #[arg(long)]
        capture_timeout_seconds: Option<u64>,
        #[arg(long)]
        refresh_before_seconds: Option<i64>,
        #[arg(long)]
        refresh_retry_seconds: Option<u64>,
        #[arg(long, help = "Disable terminal dashboard")]
        no_tui: bool,
        #[arg(long, help = "Disable menu bar / system tray icon")]
        no_tray: bool,
        #[arg(long, env = "M365_LOG_LEVEL")]
        log_level: Option<String>,
    },
    /// Verify Edge, ports, token, and CDP before serving
    Doctor {
        #[arg(long, help = "Path to config.toml")]
        config: Option<PathBuf>,
    },
    /// Paste a Substrate WebSocket URL or token
    SetToken {
        #[arg(long, help = "Path to config.toml")]
        config: Option<PathBuf>,
    },
    /// Capture token from a running debug Edge window
    CaptureToken {
        #[arg(long, help = "Path to config.toml")]
        config: Option<PathBuf>,
        #[arg(long)]
        cdp_port: Option<u16>,
        #[arg(long, default_value_t = 60)]
        timeout_seconds: u64,
    },
    /// Launch Edge with remote debugging for M365 Copilot sign-in
    LaunchEdge {
        #[arg(long, help = "Path to config.toml")]
        config: Option<PathBuf>,
        #[arg(long)]
        cdp_port: Option<u16>,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Commands::Doctor { config } => {
            let overrides = ServeOverrides {
                config_path: config,
                ..Default::default()
            };
            let _ = bootstrap(&overrides);
            let cfg = AppConfig::load(&overrides);
            let report = run_doctor(&cfg).await;
            report.print();
            if report.all_ok() {
                Ok(())
            } else {
                Err("doctor found issues".into())
            }
        }
        Commands::SetToken { config } => {
            let cfg = load_config(&ServeOverrides {
                config_path: config,
                ..Default::default()
            });
            set_token_interactive(&cfg.token.env_file)
        }
        Commands::CaptureToken {
            config,
            cdp_port,
            timeout_seconds,
        } => {
            let mut overrides = ServeOverrides {
                config_path: config,
                ..Default::default()
            };
            overrides.cdp_port = cdp_port;
            let cfg = load_config(&overrides);
            capture_token_interactive(&cfg, timeout_seconds).await
        }
        Commands::LaunchEdge { config, cdp_port } => {
            let mut overrides = ServeOverrides {
                config_path: config,
                ..Default::default()
            };
            overrides.cdp_port = cdp_port;
            let cfg = load_config(&overrides);
            launch_debug_edge_on_port(cfg.edge.cdp_port, cfg.edge.profile_dir);
            Ok(())
        }
        Commands::Serve {
            config,
            host,
            port,
            cdp_port,
            no_auto_refresh,
            no_launch_edge,
            no_capture_on_start,
            capture_timeout_seconds,
            refresh_before_seconds,
            refresh_retry_seconds,
            no_tui,
            no_tray,
            log_level,
        } => {
            let overrides = ServeOverrides {
                config_path: config,
                host,
                port,
                cdp_port,
                no_auto_refresh,
                no_launch_edge,
                no_capture_on_start,
                capture_timeout_seconds,
                refresh_before_seconds,
                refresh_retry_seconds,
                no_tui,
                no_tray,
                log_level,
            };
            match bootstrap(&overrides) {
                Err(e) => Err(e),
                Ok(_) => run_serve(overrides).await,
            }
        }
    };

    if let Err(e) = result {
        error!(error = %e, "command failed");
        eprintln!("Error: {e}");
        process::exit(1);
    }
}

fn load_config(overrides: &ServeOverrides) -> AppConfig {
    AppConfig::load(overrides)
}
