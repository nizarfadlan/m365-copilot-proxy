use std::net::TcpListener;

use reqwest::Client;

use crate::cdp::{browser_executable, browser_available, find_m365_page, needs_substrate_token, read_token_from};
use crate::config::AppConfig;
use crate::token_store::{is_substrate_token, seconds_remaining};

#[derive(Debug, Clone)]
pub struct CheckResult {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

pub struct DoctorReport {
    pub checks: Vec<CheckResult>,
}

impl DoctorReport {
    pub fn all_ok(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }

    pub fn print(&self) {
        println!("m365-copilot-proxy doctor\n");
        for check in &self.checks {
            let mark = if check.ok { "ok" } else { "FAIL" };
            println!("  [{mark}] {} — {}", check.name, check.detail);
        }
        println!();
        if self.all_ok() {
            println!("All checks passed. Run: copilot-openai-proxy serve");
        } else {
            println!("Fix the failed checks above, then run: copilot-openai-proxy serve");
        }
    }
}

pub async fn run_doctor(config: &AppConfig) -> DoctorReport {
    let mut checks = Vec::new();

    checks.push(check_config_paths(config));
    checks.push(check_browser_installed(config));
    checks.push(check_port_available(
        &config.server.host,
        config.server.port,
        "HTTP server",
    ));
    checks.push(check_port_available(
        "127.0.0.1",
        config.edge.cdp_port,
        "Browser CDP",
    ));
    checks.push(check_token(config));
    checks.push(check_cdp_reachable(config.edge.cdp_port).await);

    DoctorReport { checks }
}

fn check_config_paths(config: &AppConfig) -> CheckResult {
    let env_file = &config.token.env_file;
    let detail = format!(
        "listen {}:{} · env {} · profile {}",
        config.server.host,
        config.server.port,
        env_file.display(),
        config.edge_profile_dir().display()
    );
    CheckResult {
        name: "Configuration".into(),
        ok: true,
        detail,
    }
}

fn check_browser_installed(config: &AppConfig) -> CheckResult {
    let path = browser_executable(config);
    let ok = browser_available(config);
    CheckResult {
        name: "Chromium browser".into(),
        ok,
        detail: if ok {
            format!("found at {}", path.display())
        } else {
            format!(
                "not found at {} — install Edge/Chrome/Brave or set edge.executable in config",
                path.display()
            )
        },
    }
}

fn check_port_available(host: &str, port: u16, label: &str) -> CheckResult {
    let addr = format!("{host}:{port}");
    let ok = TcpListener::bind(&addr).is_ok();
    CheckResult {
        name: format!("{label} port"),
        ok,
        detail: if ok {
            format!("{addr} is available")
        } else {
            format!("{addr} is in use — free the port or change config")
        },
    }
}

fn check_token(config: &AppConfig) -> CheckResult {
    let token = read_token_from(&config.token.env_file);
    if needs_substrate_token(token.as_deref()) {
        return CheckResult {
            name: "Substrate token".into(),
            ok: false,
            detail: "missing or expired — sign in via browser or run set-token".into(),
        };
    }
    let token = token.unwrap();
    let remaining = seconds_remaining(&token).unwrap_or(0);
    CheckResult {
        name: "Substrate token".into(),
        ok: is_substrate_token(&token) && remaining > 0,
        detail: format!("valid · {remaining}s remaining"),
    }
}

async fn check_cdp_reachable(cdp_port: u16) -> CheckResult {
    let client = match Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return CheckResult {
                name: "Browser CDP".into(),
                ok: false,
                detail: format!("HTTP client error: {e}"),
            };
        }
    };

    match client
        .get(format!("http://127.0.0.1:{cdp_port}/json"))
        .send()
        .await
    {
        Ok(resp) => {
            let ok = resp.status().is_success();
            let detail = if ok {
                match resp.json::<Vec<serde_json::Value>>().await {
                    Ok(tabs) => {
                        if find_m365_page(&tabs).is_some() {
                            "CDP reachable · M365 Copilot tab open".into()
                        } else {
                            "CDP reachable · open https://m365.cloud.microsoft/chat in the debug browser"
                                .into()
                        }
                    }
                    Err(_) => "CDP reachable".into(),
                }
            } else {
                format!("CDP returned HTTP {}", resp.status())
            };
            CheckResult {
                name: "Browser CDP".into(),
                ok,
                detail,
            }
        }
        Err(_) => CheckResult {
            name: "Browser CDP".into(),
            ok: false,
            detail: format!(
                "not reachable on :{cdp_port} — run serve (launches browser) or launch-edge"
            ),
        },
    }
}

pub fn format_bind_error(host: &str, port: u16, err: &std::io::Error) -> String {
    if err.kind() == std::io::ErrorKind::AddrInUse {
        format!("port {host}:{port} is already in use — stop the other process or set M365_PORT")
    } else {
        format!("failed to bind {host}:{port}: {err}")
    }
}
