use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use tokio::sync::mpsc as tokio_mpsc;
use tracing::{info, warn};

use crate::config::AppConfig;
use crate::tui::UiAction;

pub struct TrayHandle {
    join: thread::JoinHandle<()>,
    shutdown: mpsc::Sender<()>,
}

impl TrayHandle {
    pub fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.join.join();
    }
}

/// macOS AppKit menus must be created on the main thread; tray-icon spawns a worker thread.
pub fn tray_supported() -> bool {
    !cfg!(target_os = "macos")
}

pub fn spawn_tray(
    config: &AppConfig,
    listen_addr: &str,
    action_tx: tokio_mpsc::Sender<UiAction>,
) -> Option<TrayHandle> {
    if !config.ui.tray {
        return None;
    }

    if !tray_supported() {
        warn!(
            "system tray disabled on macOS (AppKit requires the main thread); \
             use the TUI or pass --no-tray"
        );
        return None;
    }

    let thread_action_tx = action_tx.clone();
    let health_url = format!("http://{listen_addr}/healthz");
    let (shutdown_tx, shutdown_rx) = mpsc::channel();

    let join = thread::spawn(move || {
        if let Err(e) = run_tray_thread(&health_url, thread_action_tx, shutdown_rx) {
            warn!(error = %e, "system tray unavailable; continuing without tray icon");
        }
    });

    Some(TrayHandle {
        join,
        shutdown: shutdown_tx,
    })
}

fn run_tray_thread(
    health_url: &str,
    action_tx: tokio_mpsc::Sender<UiAction>,
    shutdown_rx: mpsc::Receiver<()>,
) -> Result<(), String> {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{TrayIconBuilder, TrayIconEvent};

    let icon = build_icon()?;
    let quit_id = MenuItem::with_id("quit", "Quit", true, None);
    let refresh_id = MenuItem::with_id("refresh", "Refresh token", true, None);
    let edge_id = MenuItem::with_id("edge", "Launch browser", true, None);
    let open_id = MenuItem::with_id("open", "Open health check", true, None);
    let separator = PredefinedMenuItem::separator();

    let menu = Menu::new();
    menu.append(&open_id).map_err(|e| e.to_string())?;
    menu.append(&refresh_id).map_err(|e| e.to_string())?;
    menu.append(&edge_id).map_err(|e| e.to_string())?;
    menu.append(&separator).map_err(|e| e.to_string())?;
    menu.append(&quit_id).map_err(|e| e.to_string())?;

    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("M365 Copilot OpenAI Proxy")
        .with_icon(icon)
        .build()
        .map_err(|e| e.to_string())?;

    info!("system tray icon active");

    let menu_rx = MenuEvent::receiver();
    let tray_rx = TrayIconEvent::receiver();
    let health_url = health_url.to_string();

    loop {
        if shutdown_rx.try_recv().is_ok() {
            break;
        }

        if let Ok(event) = menu_rx.try_recv() {
            let id = event.id.0.as_str();
            if id == quit_id.id().0.as_str() {
                let _ = action_tx.blocking_send(UiAction::Quit);
                break;
            }
            if id == refresh_id.id().0.as_str() {
                let _ = action_tx.blocking_send(UiAction::RefreshToken);
            }
            if id == edge_id.id().0.as_str() {
                let _ = action_tx.blocking_send(UiAction::LaunchEdge);
            }
            if id == open_id.id().0.as_str() {
                let _ = open::that(&health_url);
            }
        }

        if let Ok(TrayIconEvent::Click { .. }) = tray_rx.try_recv() {
            let _ = action_tx.blocking_send(UiAction::RefreshToken);
        }

        thread::sleep(Duration::from_millis(200));
    }

    Ok(())
}

fn build_icon() -> Result<tray_icon::Icon, String> {
    let width = 22u32;
    let height = 22u32;
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let edge = x == 0 || y == 0 || x == width - 1 || y == height - 1;
            if edge {
                rgba.extend_from_slice(&[30, 96, 168, 255]);
            } else {
                rgba.extend_from_slice(&[70, 140, 220, 255]);
            }
        }
    }
    tray_icon::Icon::from_rgba(rgba, width, height).map_err(|e| e.to_string())
}
