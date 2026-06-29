# Microsoft 365 Copilot OpenAI Proxy (Rust)

Rust port of [kuchris/m365-copilot-openai-proxy](https://github.com/kuchris/m365-copilot-openai-proxy).

Use Microsoft 365 Copilot through OpenAI-compatible clients. This runs a local HTTP proxy over the same `substrate.office.com` WebSocket API used by the M365 Copilot web UI.

No Azure app registration. Sign in with your normal M365 Copilot browser session.

## Install (no compile)

Download the latest binary for your OS from [GitHub Releases](https://github.com/nizarfadlan/m365-copilot-proxy/releases).

```bash
# macOS (Apple Silicon example)
curl -L -o copilot-openai-proxy.tar.gz \
  https://github.com/nizarfadlan/m365-copilot-proxy/releases/latest/download/m365-copilot-proxy-macos-aarch64.tar.gz
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

On first run the proxy opens a **Chromium browser** window for sign-in when no token exists. Once authenticated, the browser runs **headless in the background** for CDP token refresh (config: `headless_when_authenticated = true`).

Browser profile default: `~/.m365-copilot-proxy/edge-profile`

### Chromium browsers (not only Edge)

Auto-refresh uses **Chrome DevTools Protocol (CDP)**. Any Chromium-based browser works:

- Microsoft Edge (default if installed)
- Google Chrome
- Brave
- Chromium
- **Playwright** Chromium (`~/Library/Caches/ms-playwright/` on macOS)

Set explicitly in `config.toml`:

```toml
[edge]
executable = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
```

Or via env: `M365_BROWSER_EXECUTABLE=/usr/bin/google-chrome`

If unset, the proxy auto-detects the first available browser (system install or Playwright cache). A configured path is always used when it exists on disk.

### First-run setup (onboarding)

On first run, an interactive **setup wizard** runs before the proxy starts (TTY required):

```bash
copilot-openai-proxy serve          # wizard on first run
copilot-openai-proxy onboard        # re-run setup anytime
copilot-openai-proxy serve --skip-onboarding
```

The wizard lets you pick a detected browser (including Playwright), enter a custom binary path, **download Chromium** (~150 MB via Playwright CDN), set listen/CDP ports, and toggle token capture options.

```bash
# Browser step options:
#   • Detected browsers listed with full paths
#   • Custom path — type path to any Chromium binary
#   • Download Chromium — built-in installer (npx or direct download)
```

Firefox and Safari are **not** supported for auto-refresh (different debug protocols). Use `set-token` manually instead.

## Terminal UI

`serve` shows a **TUI dashboard** when stdout is a TTY:

| Key | Action |
|-----|--------|
| `q` | Quit |
| `r` | Refresh token (restarts server) |
| `e` | Launch debug browser |
| `↑`/`↓` | Scroll logs |

Disable: `--no-tui` or `M365_TUI=false` in config/env.

## System Tray

On **Windows & Linux**, when enabled (default), a system tray icon provides:

- Open health check
- Refresh token
- Launch browser
- Quit

**macOS:** tray is disabled by default — AppKit menus must run on the main thread, which conflicts with the async server + TUI. Use the TUI (`q` quit, `r` refresh) instead.

Disable: `--no-tray` or `M365_TRAY=false`.

## API Reference

Interactive docs (auto-generated from route handlers via **utoipa**):

- Swagger UI: `http://127.0.0.1:8000/docs`
- OpenAPI JSON: `http://127.0.0.1:8000/openapi.json`

Base URL: `http://127.0.0.1:8000` (change via `[server]` in config).

### Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/healthz` | Health check + Substrate token summary |
| `GET` | `/v1/token/status` | Substrate JWT status (valid / seconds remaining) |
| `GET` | `/v1/models` | OpenAI-compatible model list |
| `POST` | `/v1/chat/completions` | OpenAI Chat Completions (JSON or SSE stream) |
| `POST` | `/v1/responses` | OpenAI Responses API (JSON or SSE stream) |
| `POST` | `/v1/messages` | Anthropic Messages API (JSON or SSE stream) |

### Authentication — two layers

**1. Client → proxy (OpenAI/Anthropic SDK API key)**

The proxy **does not validate** `Authorization: Bearer …` or client `apiKey` today.
Any string works — clients use `dummy` because SDKs require a non-empty key:

```bash
export OPENAI_API_KEY="dummy"
export ANTHROPIC_API_KEY="dummy"
```

There is **no config option yet** to enforce a custom client API key. The server
listens on `127.0.0.1` by default (local only). To expose on LAN, set
`M365_HOST=0.0.0.0` and protect with a firewall or reverse proxy.

**2. Proxy → Microsoft (Substrate token)**

This is the real credential. It is the browser JWT from your M365 Copilot session,
stored in `.env` / `M365_ACCESS_TOKEN` and auto-refreshed via CDP. Without it,
chat endpoints return upstream errors.

Check token status:

```bash
curl -s http://127.0.0.1:8000/v1/token/status
curl -s http://127.0.0.1:8000/healthz
```

### Session / multi-turn context

| Mechanism | Example |
|-----------|---------|
| Model suffix | `"model": "m365-copilot:persist"` + `"user": "my-session"` |
| Header | `X-M365-Session-Id: my-session` |

Default model alias: `m365-copilot` (config: `M365_MODEL_ALIAS` / `[token].model_alias`).

### Examples

```bash
# Models
curl -s http://127.0.0.1:8000/v1/models

# Chat (OpenAI)
curl -s http://127.0.0.1:8000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -H 'Authorization: Bearer dummy' \
  -d '{"model":"m365-copilot","messages":[{"role":"user","content":"hi"}]}'

# Streaming
curl -N http://127.0.0.1:8000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"m365-copilot","stream":true,"messages":[{"role":"user","content":"hi"}]}'

# Anthropic format
curl -s http://127.0.0.1:8000/v1/messages \
  -H 'Content-Type: application/json' \
  -H 'x-api-key: dummy' \
  -d '{"model":"m365-copilot","max_tokens":1024,"messages":[{"role":"user","content":"hi"}]}'
```

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
| `M365_CDP_PORT` | Browser remote debugging port |
| `M365_AUTO_REFRESH` | `true`/`false` |
| `M365_CAPTURE_ON_START` | `true`/`false` |
| `M365_CAPTURE_TIMEOUT_SECONDS` | Startup capture timeout |
| `M365_REFRESH_BEFORE_SECONDS` | Refresh token N seconds before expiry |
| `M365_REFRESH_RETRY_SECONDS` | Retry interval on refresh failure |
| `M365_LAUNCH_EDGE` | Launch browser on start |
| `M365_BROWSER_EXECUTABLE` | Path to Edge/Chrome/Brave/Chromium binary |
| `M365_EDGE_HEADLESS_WHEN_AUTHENTICATED` | `true`/`false` — headless CDP when token valid (default `true`) |
| `M365_EDGE_PROFILE_DIR` | Browser user-data directory |
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

See [API Reference](#api-reference) for endpoint details and curl examples.

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
| API Key | any value, e.g. `dummy` (not validated by proxy) |
| Model | `m365-copilot` |
| Persistent chat | `m365-copilot:persist` or header `X-M365-Session-Id` |
| Real auth | Substrate JWT in `.env` (browser sign-in) |

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
