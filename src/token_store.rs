use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Utc};
use serde_json::Value;

pub const SUBSTRATE_AUDIENCE_PREFIX: &str = "https://substrate.office.com/";

#[derive(Debug, Clone)]
pub struct TokenStatus {
    pub valid: bool,
    pub error: Option<String>,
    pub expires_at: Option<String>,
    pub seconds_remaining: i64,
}

impl TokenStatus {
    pub fn to_json(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        map.insert("valid".into(), self.valid.into());
        if let Some(ref err) = self.error {
            map.insert("error".into(), err.clone().into());
        }
        if let Some(ref exp) = self.expires_at {
            map.insert("expires_at".into(), exp.clone().into());
        }
        map.insert(
            "seconds_remaining".into(),
            self.seconds_remaining.into(),
        );
        serde_json::Value::Object(map)
    }
}

pub fn decode_jwt_payload(token: &str) -> Result<Value, String> {
    let payload = token
        .split('.')
        .nth(1)
        .ok_or_else(|| "invalid JWT".to_string())?;
    let padded_len = payload.len() + (4 - payload.len() % 4) % 4;
    let padded = format!("{payload}{}", "=".repeat(padded_len - payload.len()));
    let bytes = URL_SAFE_NO_PAD
        .decode(padded.trim_end_matches('='))
        .map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

pub fn is_substrate_token_claims(claims: &Value) -> bool {
    claims
        .get("aud")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .starts_with(SUBSTRATE_AUDIENCE_PREFIX)
}

pub fn is_substrate_token(token: &str) -> bool {
    decode_jwt_payload(token)
        .map(|claims| is_substrate_token_claims(&claims))
        .unwrap_or(false)
}

pub fn seconds_remaining(token: &str) -> Result<i64, String> {
    let claims = decode_jwt_payload(token)?;
    let exp = claims
        .get("exp")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| "missing exp claim".to_string())?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs() as i64;
    Ok(exp - now)
}

pub fn read_env_token(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let stripped = line.trim();
        if stripped.is_empty() || stripped.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = stripped.split_once('=') {
            if key.trim() == "M365_ACCESS_TOKEN" {
                return Some(clean_env_value(value));
            }
        }
    }
    None
}

pub fn write_token(path: &Path, token: &str) -> std::io::Result<()> {
    let token_line = format!("M365_ACCESS_TOKEN={token}");
    let text = if path.exists() {
        let existing = std::fs::read_to_string(path)?;
        let has_active_line = existing.lines().any(|line| {
            let stripped = line.trim();
            !stripped.starts_with('#') && stripped.starts_with("M365_ACCESS_TOKEN=")
        });
        if has_active_line {
            existing
                .lines()
                .map(|line| {
                    let stripped = line.trim();
                    if !stripped.starts_with('#') && stripped.starts_with("M365_ACCESS_TOKEN=") {
                        token_line.as_str()
                    } else {
                        line
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        } else if existing.ends_with('\n') || existing.is_empty() {
            format!("{existing}{token_line}\n")
        } else {
            format!("{existing}\n{token_line}\n")
        }
    } else {
        format!("{token_line}\n")
    };
    std::fs::write(path, text)
}

fn clean_env_value(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'')
            || (bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
        {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

#[derive(Clone)]
pub struct AccessTokenStore {
    token: Arc<RwLock<String>>,
    env_path: PathBuf,
    mtime_ns: Arc<RwLock<Option<u128>>>,
}

impl AccessTokenStore {
    pub fn new(token: String, env_path: impl Into<PathBuf>) -> Self {
        let env_path = env_path.into();
        let mtime_ns = read_mtime_ns(&env_path);
        Self {
            token: Arc::new(RwLock::new(token)),
            env_path,
            mtime_ns: Arc::new(RwLock::new(mtime_ns)),
        }
    }

    pub fn get(&self) -> String {
        self.reload_if_changed();
        self.token.read().unwrap().clone()
    }

    pub fn status(&self) -> TokenStatus {
        let token = self.get();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let claims = match decode_jwt_payload(&token) {
            Ok(c) => c,
            Err(e) => {
                return TokenStatus {
                    valid: false,
                    error: Some(format!("Cannot decode access token: {e}")),
                    expires_at: None,
                    seconds_remaining: 0,
                };
            }
        };

        if !is_substrate_token_claims(&claims) {
            return TokenStatus {
                valid: false,
                error: Some("Access token is not a substrate.office.com token.".into()),
                expires_at: None,
                seconds_remaining: 0,
            };
        }

        let expires_at = claims.get("exp").and_then(|v| v.as_i64()).unwrap_or(0);
        let seconds_remaining = (expires_at - now).max(0);
        let expires_iso = DateTime::<Utc>::from_timestamp(expires_at, 0)
            .map(|dt| dt.to_rfc3339());

        TokenStatus {
            valid: seconds_remaining > 0,
            error: None,
            expires_at: expires_iso,
            seconds_remaining,
        }
    }

    fn reload_if_changed(&self) {
        let current_mtime = read_mtime_ns(&self.env_path);
        let should_reload = {
            let stored = self.mtime_ns.read().unwrap();
            current_mtime.is_some() && current_mtime != *stored
        };
        if !should_reload {
            return;
        }
        if let Some(token) = read_env_token(&self.env_path) {
            *self.token.write().unwrap() = token;
            *self.mtime_ns.write().unwrap() = current_mtime;
        }
    }
}

fn read_mtime_ns(path: &Path) -> Option<u128> {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
}
