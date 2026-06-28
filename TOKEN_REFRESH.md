# Token Refresh

The `substrate.office.com` API requires a browser JWT that expires in about one hour. This Rust port uses **Chrome DevTools Protocol (CDP)** on a Chromium-based browser (Edge, Chrome, Brave, Chromium).

## Automatic (default)

```bash
copilot-openai-proxy serve
```

1. Launches a Chromium browser with `--remote-debugging-port` (default `9222`) — Edge, Chrome, or Brave if auto-detected
2. Uses profile `~/.m365-copilot-proxy/edge-profile` (sign in once)
3. Captures token on startup if missing
4. Auto-refreshes when less than 5 minutes remain (configurable)

### Useful flags

```bash
copilot-openai-proxy serve --refresh-before-seconds 300
copilot-openai-proxy serve --no-launch-edge
copilot-openai-proxy serve --no-capture-on-start
copilot-openai-proxy serve --no-auto-refresh
```

### Use Chrome or Brave instead of Edge

```toml
# config.toml
[edge]
executable = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
```

Or: `export M365_BROWSER_EXECUTABLE=/usr/bin/google-chrome`

### If capture stalls

In the debug browser Copilot tab:

1. Press `F5` to reload
2. Click the message box
3. Type **one character** (do not send)

Or press `r` in the TUI / use tray **Refresh token**.

## Manual fallback

```bash
copilot-openai-proxy set-token
```

Paste the full WebSocket URL from DevTools → Network → filter `substrate` → WebSocket → Request URL.

## Check before serving

```bash
copilot-openai-proxy doctor
```

Verifies Edge install, ports, token, and CDP reachability.

## Long-term alternative

Ask IT for Entra app admin consent — then MSAL-based auth becomes possible and browser capture is unnecessary. See upstream `TOKEN_REFRESH.md` Option D.
