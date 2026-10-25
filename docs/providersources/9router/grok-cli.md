# Grok CLI (`grok-cli`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `grok-cli`
- **Priority**: `275`
- **Primary Alias**: `gcli`
- **Additional Aliases**: `["grok-build", "gb"]`
- **UI Alias**: `gcli`
- **Display Name**: Grok CLI (Grok Build)
- **Category**: `oauth`
- **Auth Modes**: `["oauth"]`
- **Website**: `https://x.ai` (`https://grok.com/supergrok`)
- **Executor**: `GrokCliExecutor` (`open-sse/executors/grok-cli.js`)
- **Source Files**:
  - `open-sse/providers/registry/grok-cli.js`
  - `open-sse/executors/grok-cli.js`
  - `open-sse/config/grokCli.js`
  - `src/lib/oauth/providers/grok-cli.js`

---

## 2. Endpoints & OAuth 2.0 Device Code Flow
- **Responses API Endpoint**: `POST https://cli-chat-proxy.grok.com/v1/responses` (`format: "openai-responses"`, `forceStream: true`)
- **Models Endpoint**: `GET https://cli-chat-proxy.grok.com/v1/models`
- **User & Subscription Endpoint**: `GET https://cli-chat-proxy.grok.com/v1/user?include=subscription`
- **Billing / Credits Endpoint**: `GET https://cli-chat-proxy.grok.com/v1/billing?format=credits`
- **OAuth 2.0 Device Code Configuration**:
  - `clientId`: `b1a00492-073a-47ea-816f-4c329264a828`
  - `deviceCodeUrl`: `POST https://auth.x.ai/oauth2/device/code` (sends `referrer=grok-build`, `User-Agent: grok-pager/1.0.44 grok-shell/1.0.44 (linux; x86_64)`)
  - `tokenUrl` / `refreshUrl`: `POST https://auth.x.ai/oauth2/token`
  - `scope`: `"openid profile email offline_access grok-cli:access api:access conversations:read conversations:write"`
  - `refreshLeadMs`: `300000` (5 minutes)

---

## 3. Headers & Request Transformations (`GrokCliExecutor`)
- **Request Headers (`buildHeaders`)**:
  - `Authorization`: `Bearer <accessToken>`
  - `User-Agent`: `grok-shell/1.0.44 (linux; x86_64)`
  - `x-grok-client-identifier`: `"grok-shell"`
  - `x-grok-client-version`: `"1.0.44"`
  - `x-grok-session-id`: `<sessionId>`
  - `x-grok-conv-id`: `<sessionId>`
  - `x-grok-req-id`: `<uuid>`
  - `x-grok-turn-idx`: `"<1-based monotonic user turn index>"` (`resolveGrokCliTurnIdx`)
  - `x-grok-agent-id`: `<deviceId || derivedMachineIdUuid>`
  - `x-grok-model-override`: `<resolvedModel>`
  - `x-email`: `<email>` (if available)
  - `x-userid`: `<userId>` (if available)
- **Request Transformations (`transformRequest`)**:
  - Preserves `role: "system"` in `body.input` (unlike OpenAI Codex, Grok CLI sends `system` rather than `developer`).
  - Strips non-native server item IDs (`/^(rs|fc|resp|msg)_/` unless matching UUID-formatted `GROK_CLI_NATIVE_ITEM_ID`) and converts `custom_tool_call` into `function_call` with `{ input: "<string>" }`.
  - Sets `body.stream = true`, `body.store = false`, `body.reasoning = { summary: "concise", ...(supportsGrokCliReasoningEffort ? { effort } : {}) }`, and adds `"reasoning.encrypted_content"` to `body.include`.
  - Filters top-level fields against `RESPONSES_API_ALLOWLIST`.

---

## 4. Supported Models
| Model ID | Display Name | Upstream Model ID | Extra Metadata |
|---|---|---|---|
| `grok-build` | Grok Build | `grok-build` | `contextLength: 500000`, `maxOutputTokens: 64000` |
| `grok-4.7` | Grok 4.7 | `grok-4.7` | — |
| `grok-4.5` | Grok 4.5 | `grok-4.5` | Supports `reasoning.effort` |
| `grok-4.5-high` | Grok 4.5 (High) | `grok-4.5` | `effort: "high"` |
| `grok-4.5-medium` | Grok 4.5 (Medium) | `grok-4.5` | `effort: "medium"` |
| `grok-4.5-low` | Grok 4.5 (Low) | `grok-4.5` | `effort: "low"` |

---

## 5. Verbatim Source Code Snippets

### `open-sse/config/grokCli.js`
```javascript
export const GROK_CLI_VERSION = "1.0.44";
export const GROK_CLI_MODEL = "grok-build";
export const GROK_CLI_BASE_URL = "https://cli-chat-proxy.grok.com/v1";
export const GROK_CLI_CLIENT_IDENTIFIER = "grok-shell";
export const GROK_CLI_USER_AGENT = `grok-shell/${GROK_CLI_VERSION} (linux; x86_64)`;
export const GROK_CLI_PAGER_USER_AGENT = `grok-pager/${GROK_CLI_VERSION} grok-shell/${GROK_CLI_VERSION} (linux; x86_64)`;

export function supportsGrokCliReasoningEffort(model) {
  return /^grok-4\.5(?:$|-)/.test(String(model || ""));
}
```

### `open-sse/executors/grok-cli.js` (`buildHeaders`)
```javascript
  buildHeaders(credentials, stream = true) {
    const headers = super.buildHeaders(credentials, stream);
    const staticHeaders = this.config.headers || {};
    for (const [k, v] of Object.entries(staticHeaders)) {
      if (v != null && headers[k] === undefined) headers[k] = v;
    }

    headers["x-grok-client-identifier"] =
      this.config.clientIdentifier || headers["x-grok-client-identifier"] || GROK_CLI_CLIENT_IDENTIFIER;
    headers["x-grok-client-version"] =
      this.config.clientVersion || headers["x-grok-client-version"] || GROK_CLI_VERSION;

    const sessionId = this._currentSessionId || credentials?.connectionId || crypto.randomUUID();
    const reqId = this._currentReqId || crypto.randomUUID();
    headers["x-grok-session-id"] = sessionId;
    headers["x-grok-conv-id"] = sessionId;
    headers["x-grok-req-id"] = reqId;
    headers["x-grok-turn-idx"] = String(this._currentTurnIdx || 1);

    if (this._agentId) headers["x-grok-agent-id"] = this._agentId;
    if (this._currentModel) headers["x-grok-model-override"] = this._currentModel;

    const psd = credentials?.providerSpecificData || {};
    const email = psd.email || credentials?.email;
    const userId = psd.userId || credentials?.userId || credentials?.providerUserId;
    if (email) headers["x-email"] = email;
    if (userId) headers["x-userid"] = userId;

    return headers;
  }
```
