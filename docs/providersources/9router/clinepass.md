# ClinePass (`clinepass`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `clinepass`
- **Priority**: `85`
- **Alias / UI Alias**: `clinepass`
- **Display Name**: ClinePass
- **Category**: `oauth`
- **Auth Modes**: `["apikey", "oauth"]`
- **Website**: `https://cline.bot`
- **Signup / API Keys URL**: `https://app.cline.bot` (`app.cline.bot/settings/api-keys`)
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source Files**:
  - `open-sse/providers/registry/clinepass.js`
  - `open-sse/shared/clineAuth.js`
  - `open-sse/shared/clineEnvelope.js`
  - `src/lib/oauth/providers/clinepass.js`

---

## 2. Endpoints & Transport
- **Chat Completions Endpoint**: `POST https://api.cline.bot/api/v1/chat/completions`
- **OAuth Endpoints**:
  - `authorizeUrl`: `https://api.cline.bot/api/v1/auth/authorize`
  - `tokenUrl`: `https://api.cline.bot/api/v1/auth/token`
  - `refreshUrl`: `https://api.cline.bot/api/v1/auth/refresh`

---

## 3. Authentication, Headers & Quirks
- **API Key vs. WorkOS JWT Distinguishing Logic (`getClineAccessToken`)**:
  - ClinePass accepts plain API keys issued from `https://app.cline.bot/settings/api-keys` (e.g., `clp_...`) as well as OAuth tokens.
  - Because prefixing a plain API key (`clp_...`) with `workos:` causes `api.cline.bot` to return HTTP 401, `getClineAccessToken` checks `/^eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+/` and only prefixes `workos:` onto actual WorkOS JWTs, leaving plain API keys untouched in `Authorization: Bearer <apiKey>`.
- **Headers Hook (`clineHeaders`)**:
  - `HTTP-Referer`: `https://cline.bot`
  - `X-Title`: `Cline`
  - `User-Agent`: `9Router/<APP_VERSION>`
  - `X-PLATFORM`, `X-PLATFORM-VERSION`, `X-CLIENT-TYPE: 9router`, `X-CLIENT-VERSION`, `X-CORE-VERSION`, `X-IS-MULTIROOT: "false"`
- **Non-Stream Envelope (`quirks.clineEnvelope: true`)**:
  - Unwraps `{"success": true, "data": { ... }}` via `unwrapClineEnvelope`.
- **Thinking Configuration**:
  - `options`: `["auto", "on", "off"]`, `defaultMode: "auto"`

---

## 4. Supported Models
| Model ID | Display Name |
|---|---|
| `cline-pass/glm-5.2` | GLM-5.2 (ClinePass) |
| `cline-pass/kimi-k2.7-code` | Kimi K2.7 Code (ClinePass) |
| `cline-pass/kimi-k2.6` | Kimi K2.6 (ClinePass) |
| `cline-pass/deepseek-v4-pro` | DeepSeek V4 Pro (ClinePass) |
| `cline-pass/deepseek-v4-flash` | DeepSeek V4 Flash (ClinePass) |
| `cline-pass/mimo-v2.5` | MiMo-V2.5 (ClinePass) |
| `cline-pass/mimo-v2.5-pro` | MiMo-V2.5-Pro (ClinePass) |
| `cline-pass/minimax-m3` | MiniMax M3 (ClinePass) |
| `cline-pass/qwen3.7-max` | Qwen3.7 Max (ClinePass) |
| `cline-pass/qwen3.7-plus` | Qwen3.7 Plus (ClinePass) |

---

## 5. Verbatim Source Code

### `open-sse/providers/registry/clinepass.js`
```javascript
export default {
  id: "clinepass",
  priority: 85,
  alias: "clinepass",
  uiAlias: "clinepass",
  display: {
    name: "ClinePass",
    icon: "vpn_key",
    color: "#5B9BD5",
    textIcon: "CP",
    website: "https://cline.bot",
    notice: {
      signupUrl: "https://app.cline.bot",
    },
  },
  category: "oauth",
  authModes: ["apikey", "oauth"],
  hasOAuth: true,
  transport: {
    baseUrl: "https://api.cline.bot/api/v1/chat/completions",
    headers: {
      "HTTP-Referer": "https://cline.bot",
      "X-Title": "Cline",
    },
    // Non-stream chat completions come back wrapped in {"success":true,"data":{...}}
    quirks: { clineEnvelope: true },
    auth: {
      combined: true,
      header: "Authorization",
      scheme: "bearer",
      hooks: [
        "clineHeaders",
      ],
    },
  },
  models: [
    { id: "cline-pass/glm-5.2", name: "GLM-5.2 (ClinePass)" },
    { id: "cline-pass/kimi-k2.7-code", name: "Kimi K2.7 Code (ClinePass)" },
    { id: "cline-pass/kimi-k2.6", name: "Kimi K2.6 (ClinePass)" },
    { id: "cline-pass/deepseek-v4-pro", name: "DeepSeek V4 Pro (ClinePass)" },
    { id: "cline-pass/deepseek-v4-flash", name: "DeepSeek V4 Flash (ClinePass)" },
    { id: "cline-pass/mimo-v2.5", name: "MiMo-V2.5 (ClinePass)" },
    { id: "cline-pass/mimo-v2.5-pro", name: "MiMo-V2.5-Pro (ClinePass)" },
    { id: "cline-pass/minimax-m3", name: "MiniMax M3 (ClinePass)" },
    { id: "cline-pass/qwen3.7-max", name: "Qwen3.7 Max (ClinePass)" },
    { id: "cline-pass/qwen3.7-plus", name: "Qwen3.7 Plus (ClinePass)" },
  ],
  oauth: {
    appBaseUrl: "https://app.cline.bot",
    apiBaseUrl: "https://api.cline.bot",
    authorizeUrl: "https://api.cline.bot/api/v1/auth/authorize",
    tokenUrl: "https://api.cline.bot/api/v1/auth/token",
    refreshUrl: "https://api.cline.bot/api/v1/auth/refresh",
  },
  thinkingConfig: {
    options: ["auto", "on", "off"],
    defaultMode: "auto",
  },
};
```
