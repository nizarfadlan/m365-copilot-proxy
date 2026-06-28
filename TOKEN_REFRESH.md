# Token Refresh

The `substrate.office.com` API requires a browser JWT that expires in about one hour. This Rust port uses the same **Edge CDP** approach as the [upstream Python proxy](https://github.com/kuchris/m365-copilot-openai-proxy/blob/main/TOKEN_REFRESH.md).

## Automatic (default)

```bash
copilot-openai-proxy serve
```

1. Launches Microsoft Edge with `--remote-debugging-port` (default `9222`)
2. Uses profile `~/.m365-copilot-openai-proxy/edge-profile` (sign in once)
3. Captures token on startup if missing
4. Auto-refreshes when less than 5 minutes remain (configurable)

### Useful flags

```bash
copilot-openai-proxy serve --refresh-before-seconds 300
copilot-openai-proxy serve --no-launch-edge
copilot-openai-proxy serve --no-capture-on-start
copilot-openai-proxy serve --no-auto-refresh
```

### If capture stalls

In the debug Edge Copilot tab:

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
