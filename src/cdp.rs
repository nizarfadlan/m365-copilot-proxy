use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use regex::Regex;
use reqwest::Client;
use serde_json::Value;
use tokio::time::sleep;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

use crate::config::AppConfig;
use crate::token_store::{is_substrate_token, read_env_token, seconds_remaining, write_token};

const CDP_JS: &str = r#"
(() => {
  const candidates = [];
  for (const store of [sessionStorage, localStorage]) {
    for (const key of ['LokiAuthToken', ...Object.keys(store).filter(k => k.startsWith('LokiAuthToken'))]) {
      const token = store.getItem(key);
      if (token && token.startsWith('eyJ')) candidates.push(token);
    }
  }
  for (const entry of performance.getEntriesByType('resource')) {
    if (!entry.name.includes('substrate.office.com') ||
        !entry.name.includes('access_token=')) continue;
    const match = entry.name.match(/[?&]access_token=([^&]+)/);
    if (match) candidates.push(decodeURIComponent(match[1]));
  }
  const stores = [sessionStorage, localStorage];
  for (const store of stores) {
    for (const k of Object.keys(store)) {
      if (!k.includes('accesstoken')) continue;
      try {
        const v = JSON.parse(store.getItem(k));
        if (v && v.secret && v.secret.startsWith('eyJ') &&
            ((v.target && v.target.includes('substrate')) || k.includes('substrate'))) {
          candidates.push(v.secret);
        }
      } catch {}
    }
  }
  return candidates;
})()
"#;

const CDP_NUDGE_JS: &str = r#"
(() => {
  const input = document.querySelector('[aria-label="Message Copilot"], textarea, [contenteditable="true"], [role="textbox"]');
  if (!input) return false;
  input.focus();
  return true;
})()
"#;

pub fn find_m365_page(tabs: &[Value]) -> Option<Value> {
    tabs.iter()
        .find(|tab| {
            tab.get("type").and_then(|v| v.as_str()) == Some("page")
                && tab
                    .get("url")
                    .and_then(|v| v.as_str())
                    .map(|url| url.starts_with("https://m365.cloud.microsoft/"))
                    .unwrap_or(false)
        })
        .cloned()
}

pub fn needs_substrate_token(token: Option<&str>) -> bool {
    match token {
        None => true,
        Some(t) if !is_substrate_token(t) => true,
        Some(t) => seconds_remaining(t).map(|r| r <= 0).unwrap_or(true),
    }
}

pub fn read_token_from(env_file: &Path) -> Option<String> {
    read_env_token(env_file)
}

pub fn write_token_to(env_file: &Path, token: &str) -> Result<(), String> {
    write_token(env_file, token).map_err(|e| e.to_string())
}

/// Backward-compatible helper for tests.
pub fn read_token() -> Option<String> {
    read_token_from(Path::new(".env"))
}

pub async fn cdp_extract_token(port: u16, allow_nudge: bool) -> Option<String> {
    let client = Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .ok()?;
    let tabs: Vec<Value> = client
        .get(format!("http://localhost:{port}/json"))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let tab = find_m365_page(&tabs)?;
    let ws_url = tab.get("webSocketDebuggerUrl")?.as_str()?;

    let (mut ws, _) = connect_async(ws_url).await.ok()?;
    ws.send(Message::Text(
        serde_json::json!({
            "id": 1,
            "method": "Runtime.evaluate",
            "params": {"expression": CDP_JS},
        })
        .to_string()
        .into(),
    ))
    .await
    .ok()?;

    if let Some(Ok(Message::Text(raw))) = ws.next().await {
        if let Ok(msg) = serde_json::from_str::<Value>(&raw) {
            if let Some(candidates) = msg
                .get("result")
                .and_then(|r| r.get("result"))
                .and_then(|r| r.get("value"))
                .and_then(|v| v.as_array())
            {
                for token in candidates {
                    if let Some(t) = token.as_str() {
                        if is_substrate_token(t) {
                            return Some(t.to_string());
                        }
                    }
                }
            }
        }
    }

    if !allow_nudge {
        return None;
    }
    cdp_nudge_and_wait_for_token(&mut ws).await
}

pub async fn cdp_capture_websocket_token(port: u16, timeout_seconds: u64) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
    while Instant::now() < deadline {
        let client = Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .ok()?;
        let tabs: Vec<Value> = match client
            .get(format!("http://localhost:{port}/json"))
            .send()
            .await
        {
            Ok(resp) => match resp.json().await {
                Ok(t) => t,
                Err(_) => {
                    sleep(Duration::from_secs(1)).await;
                    continue;
                }
            },
            Err(_) => {
                sleep(Duration::from_secs(1)).await;
                continue;
            }
        };

        let tab = match find_m365_page(&tabs) {
            Some(t) => t,
            None => {
                sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        let ws_url = match tab.get("webSocketDebuggerUrl").and_then(|v| v.as_str()) {
            Some(u) => u,
            None => continue,
        };

        let (mut ws, _) = match connect_async(ws_url).await {
            Ok(conn) => conn,
            Err(_) => {
                sleep(Duration::from_secs(1)).await;
                continue;
            }
        };

        ws.send(Message::Text(
            serde_json::json!({"id": 1, "method": "Network.enable"})
                .to_string()
                .into(),
        ))
        .await
        .ok()?;

        if let Some(token) = wait_for_substrate_websocket_token(&mut ws, deadline).await {
            return Some(token);
        }
    }
    None
}

async fn wait_for_substrate_websocket_token<S>(ws: &mut S, deadline: Instant) -> Option<String>
where
    S: futures_util::StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + futures_util::SinkExt<Message>
        + Unpin,
{
    let re = Regex::new(r"[?&]access_token=([^&]+)").unwrap();
    while Instant::now() < deadline {
        let raw = match tokio::time::timeout(Duration::from_secs(1), ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => t,
            _ => continue,
        };
        let msg: Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if msg.get("method").and_then(|v| v.as_str()) != Some("Network.webSocketCreated") {
            continue;
        }
        let url = msg
            .get("params")
            .and_then(|p| p.get("url"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !url.contains("substrate.office.com") {
            continue;
        }
        if let Some(caps) = re.captures(url) {
            let token = caps.get(1)?.as_str();
            if is_substrate_token(token) {
                return Some(token.to_string());
            }
        }
    }
    None
}

async fn cdp_nudge_and_wait_for_token<S>(ws: &mut S) -> Option<String>
where
    S: futures_util::StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + futures_util::SinkExt<Message>
        + Unpin,
{
    let re = Regex::new(r"[?&]access_token=([^&]+)").unwrap();
    ws.send(Message::Text(
        serde_json::json!({"id": 2, "method": "Network.enable"})
            .to_string()
            .into(),
    ))
    .await
    .ok()?;
    ws.send(Message::Text(
        serde_json::json!({
            "id": 3,
            "method": "Runtime.evaluate",
            "params": {"expression": CDP_NUDGE_JS},
        })
        .to_string()
        .into(),
    ))
    .await
    .ok()?;
    ws.send(Message::Text(
        serde_json::json!({"id": 4, "method": "Input.insertText", "params": {"text": " "}})
            .to_string()
            .into(),
    ))
    .await
    .ok()?;
    for (id, event_type) in [(5, "keyDown"), (6, "keyUp")] {
        ws.send(Message::Text(
            serde_json::json!({
                "id": id,
                "method": "Input.dispatchKeyEvent",
                "params": {
                    "type": event_type,
                    "windowsVirtualKeyCode": 8,
                    "nativeVirtualKeyCode": 8,
                    "key": "Backspace",
                    "code": "Backspace",
                },
            })
            .to_string()
            .into(),
        ))
        .await
        .ok()?;
    }

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let raw = match tokio::time::timeout(Duration::from_secs(1), ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => t,
            _ => continue,
        };
        let msg: Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if msg.get("method").and_then(|v| v.as_str()) != Some("Network.webSocketCreated") {
            continue;
        }
        let url = msg
            .get("params")
            .and_then(|p| p.get("url"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !url.contains("substrate.office.com") {
            continue;
        }
        if let Some(caps) = re.captures(url) {
            let token = caps.get(1)?.as_str();
            if is_substrate_token(token) {
                return Some(token.to_string());
            }
        }
    }
    None
}

pub async fn try_auto_refresh(config: &AppConfig, allow_nudge: bool) -> bool {
    if let Some(token) = cdp_extract_token(config.edge.cdp_port, allow_nudge).await {
        if write_token_to(&config.token.env_file, &token).is_ok() {
            info!("token refreshed automatically from browser");
            return true;
        }
    }
    false
}

pub async fn wait_for_m365_page(port: u16, timeout_seconds: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
    let client = Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    while Instant::now() < deadline {
        if let Ok(resp) = client
            .get(format!("http://localhost:{port}/json"))
            .send()
            .await
        {
            if let Ok(tabs) = resp.json::<Vec<Value>>().await {
                if find_m365_page(&tabs).is_some() {
                    return true;
                }
            }
        }
        sleep(Duration::from_millis(500)).await;
    }
    false
}

pub async fn startup_capture_loop(
    config: &AppConfig,
    status: Arc<crate::runtime_status::RuntimeStatus>,
) {
    use crate::runtime_status::ServicePhase;

    let port = config.edge.cdp_port;
    let timeout_seconds = config.token.capture_timeout_seconds;
    status.set_phase(ServicePhase::WaitingForEdge);
    info!("waiting for debug browser M365 tab");
    if !wait_for_m365_page(port, timeout_seconds.min(30)).await {
        warn!(
            "M365 Copilot tab not detected — open https://m365.cloud.microsoft/chat in the debug browser"
        );
    }
    info!("trying to refresh Substrate token from browser");
    if try_auto_refresh(config, true).await {
        status.set_phase(ServicePhase::Ready);
        return;
    }
    status.set_phase(ServicePhase::CapturingToken);
    warn!(
        "waiting for Substrate token — click Copilot message box and type one character if needed"
    );
    if let Some(token) = cdp_capture_websocket_token(port, timeout_seconds).await {
        if write_token_to(&config.token.env_file, &token).is_ok() {
            info!("startup token capture succeeded");
            status.set_phase(ServicePhase::Ready);
            return;
        }
    }
    status.set_phase(ServicePhase::CaptureFailed);
    warn!("startup token capture timed out; use set-token or capture-token");
}

pub fn launch_debug_edge(config: &AppConfig) {
    launch_debug_browser(config);
}

pub fn launch_debug_browser(config: &AppConfig) {
    let profile_dir = config.edge_profile_dir();
    let cdp_port = config.edge.cdp_port;
    let browser = browser_executable(config);
    spawn_debug_browser(cdp_port, &profile_dir, &browser);
}

pub fn launch_debug_edge_on_port(cdp_port: u16, profile_dir: Option<PathBuf>) {
    launch_debug_browser_on_port(cdp_port, profile_dir, None);
}

pub fn launch_debug_browser_on_port(
    cdp_port: u16,
    profile_dir: Option<PathBuf>,
    executable: Option<PathBuf>,
) {
    let profile_dir = profile_dir.unwrap_or_else(default_profile_dir);
    let browser = executable.unwrap_or_else(discover_chromium_browser);
    spawn_debug_browser(cdp_port, &profile_dir, &browser);
}

fn spawn_debug_browser(cdp_port: u16, profile_dir: &Path, browser: &Path) {
    std::fs::create_dir_all(profile_dir).ok();
    let mut cmd = Command::new(browser);
    cmd.arg(format!("--remote-debugging-port={cdp_port}"))
        .arg(format!("--user-data-dir={}", profile_dir.display()))
        .arg("--no-first-run")
        .arg("https://m365.cloud.microsoft/chat")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match cmd.spawn() {
        Ok(_) => info!(
            cdp_port,
            browser = %browser.display(),
            profile = %profile_dir.display(),
            "launched Chromium browser with remote debugging"
        ),
        Err(e) => warn!(
            browser = %browser.display(),
            error = %e,
            "failed to launch browser"
        ),
    }
}

fn default_profile_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".m365-copilot-proxy")
        .join("edge-profile")
}

pub fn browser_executable(config: &AppConfig) -> PathBuf {
    config
        .edge
        .executable
        .clone()
        .unwrap_or_else(discover_chromium_browser)
}

pub fn edge_executable_path() -> PathBuf {
    discover_chromium_browser()
}

pub fn edge_executable_path_for(config: &AppConfig) -> PathBuf {
    browser_executable(config)
}

pub fn edge_available() -> bool {
    chromium_browser_available(None)
}

pub fn browser_available(config: &AppConfig) -> bool {
    chromium_browser_available(config.edge.executable.as_deref())
}

fn chromium_browser_available(executable: Option<&Path>) -> bool {
    if let Some(path) = executable {
        return path.exists() || command_exists(path);
    }
    discover_chromium_browser_if_present().is_some()
}

pub fn discover_chromium_browser() -> PathBuf {
    discover_chromium_browser_if_present().unwrap_or_else(default_chromium_path)
}

fn discover_chromium_browser_if_present() -> Option<PathBuf> {
    for candidate in chromium_install_paths() {
        if candidate.exists() {
            return Some(candidate);
        }
    }
    for name in chromium_path_names() {
        if command_exists(Path::new(name)) {
            return Some(PathBuf::from(name));
        }
    }
    None
}

fn command_exists(path: &Path) -> bool {
    if path.is_absolute() || path.components().count() > 1 {
        return path.exists();
    }
    let program = path.to_string_lossy();
    #[cfg(windows)]
    {
        std::process::Command::new("where")
            .arg(program.as_ref())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("which")
            .arg(program.as_ref())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

fn chromium_install_paths() -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        vec![
            PathBuf::from(r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"),
            PathBuf::from(r"C:\Program Files\Microsoft\Edge\Application\msedge.exe"),
            PathBuf::from(r"C:\Program Files\Google\Chrome\Application\chrome.exe"),
            PathBuf::from(r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe"),
            PathBuf::from(r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe"),
            PathBuf::from(
                r"C:\Program Files (x86)\BraveSoftware\Brave-Browser\Application\brave.exe",
            ),
        ]
    }
    #[cfg(target_os = "macos")]
    {
        vec![
            PathBuf::from("/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"),
            PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
            PathBuf::from("/Applications/Brave Browser.app/Contents/MacOS/Brave Browser"),
            PathBuf::from("/Applications/Chromium.app/Contents/MacOS/Chromium"),
        ]
    }
    #[cfg(target_os = "linux")]
    {
        vec![
            PathBuf::from("/usr/bin/microsoft-edge"),
            PathBuf::from("/usr/bin/microsoft-edge-stable"),
            PathBuf::from("/usr/bin/google-chrome"),
            PathBuf::from("/usr/bin/google-chrome-stable"),
            PathBuf::from("/usr/bin/chromium"),
            PathBuf::from("/usr/bin/chromium-browser"),
            PathBuf::from("/usr/bin/brave-browser"),
            PathBuf::from("/snap/bin/chromium"),
        ]
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        vec![]
    }
}

fn chromium_path_names() -> &'static [&'static str] {
    #[cfg(target_os = "windows")]
    {
        &["msedge", "chrome", "brave"]
    }
    #[cfg(target_os = "linux")]
    {
        &[
            "microsoft-edge",
            "microsoft-edge-stable",
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
            "brave-browser",
        ]
    }
    #[cfg(target_os = "macos")]
    {
        &[]
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        &["msedge", "chrome", "chromium"]
    }
}

fn default_chromium_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        PathBuf::from(r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe")
    }
    #[cfg(target_os = "macos")]
    {
        PathBuf::from("/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge")
    }
    #[cfg(target_os = "linux")]
    {
        PathBuf::from("google-chrome")
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        PathBuf::from("msedge")
    }
}

pub fn set_token_from_input(raw: &str, env_file: &Path) -> Result<(), String> {
    let re = Regex::new(r"access_token=([^&\s]+)").unwrap();
    let token = re
        .captures(raw)
        .and_then(|c| c.get(1).map(|m| m.as_str()))
        .unwrap_or(raw.trim());
    if !token.starts_with("eyJ") {
        return Err(
            "could not find a valid token. Make sure you copied the full WebSocket URL.".into(),
        );
    }
    if !is_substrate_token(token) {
        return Err(
            "token is not a substrate.office.com WebSocket token. Copy the full wss://substrate.office.com/... URL from the Network WebSocket request.".into(),
        );
    }
    write_token_to(env_file, token)?;
    info!(path = %env_file.display(), "access token saved");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;

    #[test]
    fn browser_executable_uses_config_override() {
        let mut config = AppConfig::default();
        config.edge.executable = Some(PathBuf::from("/custom/chrome"));
        assert_eq!(browser_executable(&config), PathBuf::from("/custom/chrome"));
    }

    #[test]
    fn browser_available_checks_config_path() {
        let mut config = AppConfig::default();
        config.edge.executable = Some(PathBuf::from("/definitely/missing-browser"));
        assert!(!browser_available(&config));
    }
}
