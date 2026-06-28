# Microsoft 365 Copilot OpenAI Proxy (Rust)

Rust port of [kuchris/m365-copilot-openai-proxy](https://github.com/kuchris/m365-copilot-openai-proxy).

Use Microsoft 365 Copilot through OpenAI-compatible clients. This runs a local HTTP proxy over the same `substrate.office.com` WebSocket API used by the M365 Copilot web UI.

No Azure app registration. Sign in with your normal M365 Copilot browser session.

## Install (no compile)

Download the latest binary for your OS from [GitHub Releases](https://github.com/YOUR_ORG/m365-copilot-proxy/releases) (after you publish a tag like `v0.1.0`).

```bash
# macOS (Apple Silicon example)
curl -L -o copilot-openai-proxy.tar.gz \
  https://github.com/YOUR_ORG/m365-copilot-proxy/releases/latest/download/m365-copilot-proxy-macos-aarch64.tar.gz
tar xzf copilot-openai-proxy.tar.gz
chmod +x copilot-openai-proxy
./copilot-openai-proxy serve
```

Or build from source:

```bash
cargo build --release
./target/release/copilot-openai-proxy serve
```

## Quick Start

```bash
cp config.example.toml config.toml   # optional
copilot-openai-proxy serve
```

Server: `http://127.0.0.1:8000`

On first run the proxy opens a dedicated Edge window. Sign in once; the Substrate token is saved to `.env` (or the path in config).

Edge profile default: `~/.m365-copilot-openai-proxy/edge-profile`

## Terminal UI

`serve` shows a **TUI dashboard** when stdout is a TTY:

| Key | Action |
|-----|--------|
| `q` | Quit |
| `r` | Refresh token (restarts server) |
| `e` | Launch debug Edge |
| `↑`/`↓` | Scroll logs |

Disable: `--no-tui` or `M365_TUI=false` in config/env.

## System Tray

When enabled (default), a menu bar icon (macOS) / system tray icon (Windows & Linux) provides:

- Open health check
- Refresh token
- Launch Edge
- Quit

Disable: `--no-tray` or `M365_TRAY=false`.

## Configuration

Precedence: **CLI flags → environment variables → `config.toml` → defaults**.

### Config file locations (first match wins)

1. `--config /path/to/config.toml`
2. `M365_CONFIG` env var
3. `./config.toml`
4. `~/.config/m365-copilot-proxy/config.toml`

See [`config.example.toml`](config.example.toml) for all options.

### Environment variables

| Variable | Description |
|----------|-------------|
| `M365_ACCESS_TOKEN` | Browser WebSocket token |
| `M365_ENV_FILE` | Token file path (default `.env`) |
| `M365_HOST` | Listen host |
| `M365_PORT` | Listen port |
| `M365_TIME_ZONE` | Time zone sent to Copilot |
| `M365_MODEL_ALIAS` | Model name in `/v1/models` |
| `M365_CDP_PORT` | Edge remote debugging port |
| `M365_AUTO_REFRESH` | `true`/`false` |
| `M365_CAPTURE_ON_START` | `true`/`false` |
| `M365_CAPTURE_TIMEOUT_SECONDS` | Startup capture timeout |
| `M365_REFRESH_BEFORE_SECONDS` | Refresh token N seconds before expiry |
| `M365_REFRESH_RETRY_SECONDS` | Retry interval on refresh failure |
| `M365_LAUNCH_EDGE` | Launch Edge on start |
| `M365_EDGE_PROFILE_DIR` | Edge user-data directory |
| `M365_LOG_LEVEL` | `trace`, `debug`, `info`, `warn`, `error` |
| `M365_LOG_FORMAT` | `pretty`, `compact`, `json` |
| `M365_TUI` | Enable terminal dashboard |
| `M365_TRAY` | Enable system tray |
| `M365_CONFIG` | Path to config.toml |
| `RUST_LOG` | Alternative log filter (tracing) |

## Logging

Structured logs via [tracing](https://docs.rs/tracing). HTTP requests log method, URI, status, and latency.

```bash
M365_LOG_LEVEL=debug copilot-openai-proxy serve
M365_LOG_FORMAT=json copilot-openai-proxy serve --no-tui
```

Recent log lines also appear in the TUI panel.

## Commands

```bash
copilot-openai-proxy doctor          # preflight: Edge, ports, token, CDP
copilot-openai-proxy serve
copilot-openai-proxy serve --no-auto-refresh --no-launch-edge
copilot-openai-proxy set-token
copilot-openai-proxy capture-token
copilot-openai-proxy launch-edge
```

### Connect clients (same as upstream)

**Claude Code**

```bash
export ANTHROPIC_BASE_URL="http://127.0.0.1:8000"
export ANTHROPIC_API_KEY="dummy"
claude
```

**Continue** — add to `~/.continue/config.json`:

```json
{
  "models": [{
    "title": "M365 Copilot",
    "provider": "openai",
    "model": "m365-copilot:persist",
    "apiBase": "http://127.0.0.1:8000/v1",
    "apiKey": "dummy"
  }]
}
```

See [upstream README](https://github.com/kuchris/m365-copilot-openai-proxy) for OpenCode and more examples.

## Test

```bash
curl -s http://127.0.0.1:8000/healthz
curl -s http://127.0.0.1:8000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"m365-copilot","messages":[{"role":"user","content":"hi"}]}'
```

## Client settings

| Setting | Value |
|---------|-------|
| Base URL | `http://127.0.0.1:8000/v1` |
| API Key | `dummy` |
| Model | `m365-copilot` |
| Persistent | `m365-copilot:persist` or header `X-M365-Session-Id` |

## CI / Releases

- **CI**: tests, `fmt`, `clippy` on push/PR (`.github/workflows/ci.yml`)
- **Release**: cross-platform binaries on tag `v*` (`.github/workflows/release.yml`)

```bash
git tag v0.1.0 && git push origin v0.1.0
```

## Upstream reference

Behavior and API follow the original Python project:

- [README](https://github.com/kuchris/m365-copilot-openai-proxy/blob/main/README.md)
- [TOKEN_REFRESH.md](https://github.com/kuchris/m365-copilot-openai-proxy/blob/main/TOKEN_REFRESH.md)

## License

Apache-2.0 (same as upstream).
