use std::path::Path;
use std::sync::Arc;

use tokio::sync::{mpsc, watch};
use tracing::{error, info, warn};

use crate::bootstrap::{bootstrap, print_welcome};
use crate::cdp::{
    launch_debug_edge, needs_substrate_token, read_token_from, startup_capture_loop,
    try_auto_refresh, write_token_to,
};
use crate::config::{apply_cli_overrides, AppConfig, ServeOverrides};
use crate::doctor::format_bind_error;
use crate::logging::{init_logging, log_banner, LogBuffer};
use crate::routes::{create_router, default_app_state};
use crate::runtime_status::{RuntimeStatus, ServicePhase};
use crate::token_store::AccessTokenStore;
use crate::tray::spawn_tray;
use crate::tui::{run_tui, TuiContext, UiAction};

pub async fn run_serve(overrides: ServeOverrides) -> Result<(), String> {
    let config = AppConfig::load(&overrides);
    run_serve_with_config(overrides, config).await
}

pub async fn run_serve_with_config(
    overrides: ServeOverrides,
    mut config: AppConfig,
) -> Result<(), String> {
    apply_cli_overrides(&mut config, &overrides);
    let bootstrap_report = bootstrap(&overrides)?;
    let log_buffer = LogBuffer::new();
    init_logging(&config.logging, log_buffer.clone())?;
    log_banner();
    print_welcome(&bootstrap_report, &config);

    let runtime_status = Arc::new(RuntimeStatus::new());
    if needs_substrate_token(read_token_from(&config.token.env_file).as_deref()) {
        runtime_status.set_phase(ServicePhase::WaitingForEdge);
    } else {
        runtime_status.set_phase(ServicePhase::Ready);
    }

    info!(
        listen = %config.listen_addr(),
        model = %config.token.model_alias,
        tui = config.ui.tui,
        tray = config.ui.tray,
        "starting proxy server"
    );

    loop {
        if config.edge.launch_on_start {
            launch_debug_edge(&config);
        }

        let settings = config.settings();
        let token_store = Arc::new(AccessTokenStore::new(
            settings.access_token.clone(),
            config.token.env_file.clone(),
        ));
        let state = default_app_state(
            settings.clone(),
            config.token.env_file.clone(),
            token_store.clone(),
        );
        let app = create_router(state);
        let listen_addr = config.listen_addr();

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (action_tx, mut action_rx) = mpsc::channel::<UiAction>(8);

        if config.token.capture_on_start
            && needs_substrate_token(read_token_from(&config.token.env_file).as_deref())
        {
            let cfg = config.clone();
            let status = runtime_status.clone();
            tokio::spawn(async move {
                startup_capture_loop(&cfg, status).await;
            });
        } else {
            runtime_status.set_phase(ServicePhase::Ready);
        }

        if config.token.auto_refresh {
            let cfg = config.clone();
            let mut shutdown_clone = shutdown_rx.clone();
            tokio::spawn(async move {
                auto_refresh_loop(&cfg, &mut shutdown_clone).await;
            });
        }

        let listener = match tokio::net::TcpListener::bind(&listen_addr).await {
            Ok(l) => l,
            Err(e) => {
                return Err(format_bind_error(
                    &config.server.host,
                    config.server.port,
                    &e,
                ));
            }
        };
        info!(%listen_addr, "HTTP server listening");

        let mut server_shutdown = shutdown_rx.clone();
        let server = axum::serve(listener, app).with_graceful_shutdown(async move {
            let _ = server_shutdown.changed().await;
        });
        let server_handle = tokio::spawn(async move {
            if let Err(e) = server.await {
                error!(error = %e, "HTTP server stopped with error");
            }
        });

        let _tray = spawn_tray(config.clone(), listen_addr.clone(), action_tx.clone());

        let tui_ctx = TuiContext {
            config: config.clone(),
            token_store: token_store.clone(),
            log_buffer: log_buffer.clone(),
            listen_addr: listen_addr.clone(),
            runtime_status: runtime_status.clone(),
        };
        let tui_action_tx = action_tx.clone();
        let tui_handle = tokio::spawn(async move {
            if let Err(e) = run_tui(tui_ctx, tui_action_tx).await {
                error!(error = %e, "TUI exited with error");
            }
        });

        let mut restart = false;
        while let Some(action) = action_rx.recv().await {
            match action {
                UiAction::Quit => {
                    info!("shutdown requested");
                    break;
                }
                UiAction::RefreshToken => {
                    info!("manual token refresh requested");
                    restart = true;
                    break;
                }
                UiAction::LaunchEdge => {
                    info!("launching debug Edge window");
                    launch_debug_edge(&config);
                }
            }
        }

        let _ = shutdown_tx.send(true);
        let _ = server_handle.await;
        tui_handle.abort();

        if restart {
            info!("refreshing token from Edge");
            runtime_status.set_phase(ServicePhase::CapturingToken);
            if !try_auto_refresh(&config, true).await {
                warn!("auto-refresh failed; use set-token or capture-token");
                runtime_status.set_phase(ServicePhase::CaptureFailed);
            } else {
                runtime_status.set_phase(ServicePhase::Ready);
            }
            continue;
        }
        break;
    }

    info!("proxy stopped");
    Ok(())
}

async fn auto_refresh_loop(config: &AppConfig, shutdown_rx: &mut watch::Receiver<bool>) {
    loop {
        if *shutdown_rx.borrow() {
            break;
        }

        let token = read_token_from(&config.token.env_file);
        let Some(token) = token else {
            sleep_or_shutdown(config.token.refresh_retry_seconds, shutdown_rx).await;
            continue;
        };

        let remaining = match crate::token_store::seconds_remaining(&token) {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "auto-refresh skipped: cannot decode token");
                sleep_or_shutdown(config.token.refresh_retry_seconds, shutdown_rx).await;
                continue;
            }
        };

        if remaining > config.token.refresh_before_seconds {
            let wait = (remaining - config.token.refresh_before_seconds).min(300) as u64;
            sleep_or_shutdown(wait, shutdown_rx).await;
            continue;
        }

        info!(
            seconds_remaining = remaining.max(0),
            "token expiring soon; refreshing from Edge"
        );
        if !try_auto_refresh(config, true).await {
            warn!("auto-refresh failed; will retry later");
        }

        sleep_or_shutdown(config.token.refresh_retry_seconds, shutdown_rx).await;
    }
}

async fn sleep_or_shutdown(seconds: u64, shutdown_rx: &mut watch::Receiver<bool>) {
    tokio::select! {
        _ = shutdown_rx.changed() => {}
        _ = tokio::time::sleep(std::time::Duration::from_secs(seconds)) => {}
    }
}

pub fn set_token_interactive(env_file: &Path) -> Result<(), String> {
    println!("Paste the full WebSocket URL (or just the access_token value), then press Enter:");
    let mut raw = String::new();
    std::io::stdin()
        .read_line(&mut raw)
        .map_err(|e| e.to_string())?;
    crate::cdp::set_token_from_input(&raw, env_file)
}

pub async fn capture_token_interactive(
    config: &AppConfig,
    timeout_seconds: u64,
) -> Result<(), String> {
    info!("listening for Substrate WebSocket token via Edge CDP");
    println!("Listening for a Substrate WebSocket token...");
    println!("In the debug Edge M365 Copilot tab, click the message box and type one character.");
    match crate::cdp::cdp_capture_websocket_token(config.edge.cdp_port, timeout_seconds).await {
        Some(token) => {
            write_token_to(&config.token.env_file, &token)?;
            info!("token captured and saved");
            println!(".env updated with Substrate token.");
            Ok(())
        }
        None => Err("no Substrate WebSocket token captured before timeout".into()),
    }
}
