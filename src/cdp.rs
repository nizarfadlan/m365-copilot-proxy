use std::collections::HashSet;
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
    let browser = resolve_browser_executable(executable.as_deref());
    spawn_debug_browser(cdp_port, &profile_dir, &browser);
}

fn spawn_debug_browser(cdp_port: u16, profile_dir: &Path, browser: &Path) {
    std::fs::create_dir_all(profile_dir).ok();
    let browser = resolve_browser_executable(Some(browser));
    let mut cmd = Command::new(&browser);
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
    resolve_browser_executable(config.edge.executable.as_deref())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedBrowser {
    pub label: String,
    pub path: PathBuf,
}

/// All Chromium browsers found on this system (system installs, Playwright cache, etc.).
pub fn list_detected_browsers() -> Vec<DetectedBrowser> {
    let mut browsers = Vec::new();
    let mut seen = HashSet::new();

    for candidate in chromium_install_paths() {
        if candidate.exists() && seen.insert(candidate.clone()) {
            browsers.push(DetectedBrowser {
                label: system_browser_label(&candidate),
                path: candidate,
            });
        }
    }

    for name in chromium_path_names() {
        let path = PathBuf::from(name);
        if command_exists(&path) {
            let resolved = if path.is_absolute() {
                path.clone()
            } else {
                path
            };
            if seen.insert(resolved.clone()) {
                browsers.push(DetectedBrowser {
                    label: format!("{name} (PATH)"),
                    path: resolved,
                });
            }
        }
    }

    for (label, path) in discover_playwright_chromium_binaries() {
        if seen.insert(path.clone()) {
            browsers.push(DetectedBrowser { label, path });
        }
    }

    for path in discover_chromium_via_mdfind_all() {
        if seen.insert(path.clone()) {
            browsers.push(DetectedBrowser {
                label: system_browser_label(&path),
                path,
            });
        }
    }

    browsers
}

/// Resolve a Chromium browser binary: honor a configured path when it exists,
/// otherwise auto-detect an installed Edge/Chrome/Brave/Chromium.
fn resolve_browser_executable(configured: Option<&Path>) -> PathBuf {
    if let Some(path) = configured {
        if browser_path_usable(path) {
            return path.to_path_buf();
        }
        if let Some(found) = discover_chromium_browser_if_present() {
            warn!(
                configured = %path.display(),
                resolved = %found.display(),
                "configured browser not found; using auto-detected browser"
            );
            return found;
        }
        warn!(
            configured = %path.display(),
            "configured browser not found; no Chromium browser detected on this system"
        );
        return path.to_path_buf();
    }
    discover_chromium_browser()
}

fn browser_path_usable(path: &Path) -> bool {
    path.exists() || command_exists(path)
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
    browser_path_usable(&resolve_browser_executable(executable))
}

pub fn discover_chromium_browser() -> PathBuf {
    discover_chromium_browser_if_present().unwrap_or_else(default_chromium_path)
}

fn discover_chromium_browser_if_present() -> Option<PathBuf> {
    list_detected_browsers().into_iter().next().map(|b| b.path)
}

fn discover_chromium_via_mdfind_all() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        const BUNDLE_IDS: &[&str] = &[
            "com.microsoft.edgemac",
            "com.google.Chrome",
            "com.brave.Browser",
            "org.chromium.Chromium",
        ];
        let mut found = Vec::new();
        for bundle_id in BUNDLE_IDS {
            let Ok(output) = Command::new("mdfind")
                .arg(format!("kMDItemCFBundleIdentifier == '{bundle_id}'"))
                .output()
            else {
                continue;
            };
            if !output.status.success() {
                continue;
            }
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                let app_path = line.trim();
                if app_path.is_empty() {
                    continue;
                }
                let binary = macos_app_binary_path(app_path);
                if binary.exists() {
                    found.push(binary);
                }
            }
        }
        found
    }
    #[cfg(not(target_os = "macos"))]
    {
        vec![]
    }
}

fn discover_playwright_chromium_binaries() -> Vec<(String, PathBuf)> {
    let Some(cache) = playwright_cache_dir() else {
        return vec![];
    };
    if !cache.is_dir() {
        return vec![];
    }
    let Ok(entries) = std::fs::read_dir(&cache) else {
        return vec![];
    };

    let mut dirs: Vec<_> = entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("chromium-") && !name.contains("headless_shell")
        })
        .collect();
    dirs.sort_by(|a, b| {
        playwright_revision(&b.file_name()).cmp(&playwright_revision(&a.file_name()))
    });

    let mut found = Vec::new();
    for entry in dirs {
        let dir = entry.path();
        let revision = entry.file_name().to_string_lossy().to_string();
        if let Some(path) = playwright_chromium_binary_in_dir(&dir) {
            found.push((format!("Playwright {revision}"), path));
        }
    }
    found
}

fn playwright_revision(name: &std::ffi::OsStr) -> u64 {
    name.to_string_lossy()
        .strip_prefix("chromium-")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

pub fn playwright_chromium_binary_in_dir(dir: &Path) -> Option<PathBuf> {
    for relative in playwright_chromium_layout()
        .ok()?
        .binary_relative_paths
        .iter()
        .map(PathBuf::from)
    {
        let candidate = dir.join(relative);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// Playwright uses different names for CDN zip archives vs folders inside the zip.
/// e.g. download `chromium-mac-arm64.zip` → extract `chrome-mac-arm64/...`
#[derive(Debug, Clone, Copy)]
pub struct PlaywrightChromiumLayout {
    pub download_archive: &'static str,
    pub binary_relative_paths: &'static [&'static str],
}

pub fn playwright_chromium_layout() -> Result<PlaywrightChromiumLayout, String> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok(PlaywrightChromiumLayout {
            download_archive: "chromium-mac-arm64",
            binary_relative_paths: &[
                "chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
                "chrome-mac-arm64/Chromium.app/Contents/MacOS/Chromium",
                "chrome-mac/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
                "chrome-mac/Chromium.app/Contents/MacOS/Chromium",
            ],
        })
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Ok(PlaywrightChromiumLayout {
            download_archive: "chromium-mac",
            binary_relative_paths: &[
                "chrome-mac/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
                "chrome-mac/Chromium.app/Contents/MacOS/Chromium",
                "chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
            ],
        })
    } else if cfg!(target_os = "linux") {
        Ok(PlaywrightChromiumLayout {
            download_archive: "chromium-linux",
            binary_relative_paths: &["chrome-linux/chrome"],
        })
    } else if cfg!(target_os = "windows") {
        Ok(PlaywrightChromiumLayout {
            download_archive: "chromium-win64",
            binary_relative_paths: &["chrome-win/chrome.exe"],
        })
    } else {
        Err("unsupported platform for Playwright Chromium".into())
    }
}

pub fn playwright_chromium_relative_paths() -> Vec<PathBuf> {
    playwright_chromium_layout()
        .map(|layout| {
            layout
                .binary_relative_paths
                .iter()
                .map(|p| PathBuf::from(*p))
                .collect()
        })
        .unwrap_or_default()
}

pub fn playwright_cache_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        dirs::cache_dir().map(|d| d.join("ms-playwright"))
    }
    #[cfg(target_os = "linux")]
    {
        std::env::var("XDG_CACHE_HOME")
            .ok()
            .map(PathBuf::from)
            .or_else(dirs::cache_dir)
            .map(|d| d.join("ms-playwright"))
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var("LOCALAPPDATA")
            .ok()
            .map(|d| PathBuf::from(d).join("ms-playwright"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

fn system_browser_label(path: &Path) -> String {
    let file = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("Chromium");
    if path.to_string_lossy().contains("ms-playwright") {
        return format!("{file} (Playwright)");
    }
    match file {
        "Microsoft Edge" => "Microsoft Edge".into(),
        "Google Chrome" => "Google Chrome".into(),
        "Brave Browser" => "Brave".into(),
        "Chromium" | "Google Chrome for Testing" => format!("{file} (system)"),
        _ => file.into(),
    }
}

#[cfg(target_os = "macos")]
fn macos_app_binary_path(app_path: &str) -> PathBuf {
    let app_name = Path::new(app_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("App");
    PathBuf::from(app_path).join(format!("Contents/MacOS/{app_name}"))
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
        let mut paths = vec![
            PathBuf::from("/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"),
            PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
            PathBuf::from("/Applications/Brave Browser.app/Contents/MacOS/Brave Browser"),
            PathBuf::from("/Applications/Chromium.app/Contents/MacOS/Chromium"),
        ];
        if let Some(home) = dirs::home_dir() {
            let user_apps = home.join("Applications");
            paths.extend([
                user_apps.join("Microsoft Edge.app/Contents/MacOS/Microsoft Edge"),
                user_apps.join("Google Chrome.app/Contents/MacOS/Google Chrome"),
                user_apps.join("Brave Browser.app/Contents/MacOS/Brave Browser"),
                user_apps.join("Chromium.app/Contents/MacOS/Chromium"),
            ]);
        }
        paths
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
    fn browser_executable_uses_valid_config_override() {
        let dir = tempfile::tempdir().unwrap();
        let fake_browser = dir.path().join("chrome");
        std::fs::write(&fake_browser, b"").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake_browser, std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }

        let mut config = AppConfig::default();
        config.edge.executable = Some(fake_browser.clone());
        assert_eq!(browser_executable(&config), fake_browser);
    }

    #[test]
    fn browser_executable_falls_back_when_configured_path_missing() {
        let mut config = AppConfig::default();
        config.edge.executable = Some(PathBuf::from("/definitely/missing-browser-xyz"));
        let resolved = browser_executable(&config);
        if let Some(found) = discover_chromium_browser_if_present() {
            assert_eq!(resolved, found);
            assert!(browser_path_usable(&resolved));
        } else {
            assert_eq!(resolved, PathBuf::from("/definitely/missing-browser-xyz"));
        }
    }

    #[test]
    fn discovers_playwright_chromium_layout() {
        let layout = playwright_chromium_layout().expect("platform layout");
        let relative = PathBuf::from(layout.binary_relative_paths[0]);

        let dir = tempfile::tempdir().unwrap();
        let chromium_dir = dir.path().join("chromium-9999");
        let binary = chromium_dir.join(&relative);
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, b"").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        assert_eq!(
            playwright_chromium_binary_in_dir(&chromium_dir).as_deref(),
            Some(binary.as_path())
        );
    }

    #[test]
    fn browser_available_checks_config_path() {
        let mut config = AppConfig::default();
        config.edge.executable = Some(PathBuf::from("/definitely/missing-browser-xyz"));
        assert_eq!(
            browser_available(&config),
            discover_chromium_browser_if_present().is_some()
        );
    }
}
