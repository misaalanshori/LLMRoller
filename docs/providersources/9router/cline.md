# Cline (`cline`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `cline`
- **Priority**: `80`
- **Alias / UI Alias**: `cl`
- **Display Name**: Cline
- **Category**: `oauth`
- **Auth Modes**: `["oauth"]`
- **Website**: `https://cline.bot`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source Files**:
  - `open-sse/providers/registry/cline.js`
  - `open-sse/shared/clineAuth.js`
  - `open-sse/shared/clineEnvelope.js`
  - `src/lib/oauth/providers/cline.js`

---

## 2. Endpoints & OAuth Flow
- **Chat Completions Endpoint**: `POST https://api.cline.bot/api/v1/chat/completions`
- **OAuth Authorize Endpoint**: `https://api.cline.bot/api/v1/auth/authorize?client_type=extension&callback_url=<redirectUri>&redirect_uri=<redirectUri>`
- **Token Exchange Endpoint**: `POST https://api.cline.bot/api/v1/auth/token`
  - Note: Cline's browser callback encodes the token JSON (`{ accessToken, refreshToken, email, firstName, lastName, expiresAt }`) directly inside the `code` query parameter as Base64. `exchangeToken` first attempts to Base64-decode `code` and parse the embedded JSON object; if that fails, it falls back to `POST https://api.cline.bot/api/v1/auth/token` with `{ grant_type: "authorization_code", code, client_type: "extension", redirect_uri }`.
- **Token Refresh Endpoint**: `POST https://api.cline.bot/api/v1/auth/refresh`
  - Refresh body (`DefaultExecutor.refreshClineToken`): `{ refreshToken: "<refreshToken>", grantType: "refresh_token" }`
  - Refresh response: `{ data: { accessToken, refreshToken, expiresAt } }`

---

## 3. Authentication, Headers & Quirks
- **`workos:` JWT Prefixing (`getClineAccessToken`)**:
  - Cline OAuth access tokens are WorkOS JWTs (`eyJ...`). `getClineAccessToken` inspects the token: if it matches `/^eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+/` and does not already start with `workos:`, it prepends `workos:` so the header becomes `Authorization: Bearer workos:<jwt>`.
- **Headers Hook (`clineHeaders` -> `buildClineHeaders`)**:
  - `Authorization`: `Bearer workos:<jwt>` (or raw token if non-JWT)
  - `HTTP-Referer`: `https://cline.bot`
  - `X-Title`: `Cline`
  - `User-Agent`: `9Router/<APP_VERSION>`
  - `X-PLATFORM`: `process.platform`
  - `X-PLATFORM-VERSION`: `process.version`
  - `X-CLIENT-TYPE`: `9router`
  - `X-CLIENT-VERSION`: `<APP_VERSION>`
  - `X-CORE-VERSION`: `<APP_VERSION>`
  - `X-IS-MULTIROOT`: `"false"`
- **Non-Stream Response Envelope (`quirks.clineEnvelope: true`)**:
  - Non-streaming responses from `https://api.cline.bot/api/v1/chat/completions` are wrapped in `{"success": true, "data": { ...OpenAI chat completion... }}`.
  - `unwrapClineEnvelope(body, provider)` in `open-sse/shared/clineEnvelope.js` unwraps `body.data` when `body.success === true`.

---

## 4. Supported Models
| Model ID | Display Name |
|---|---|
| `anthropic/claude-opus-4.7` | Claude Opus 4.7 |
| `anthropic/claude-sonnet-4.6` | Claude Sonnet 4.6 |
| `anthropic/claude-opus-4.6` | Claude Opus 4.6 |
| `openai/gpt-5.3-codex` | GPT-5.3 Codex |
| `openai/gpt-5.4` | GPT-5.4 |
| `google/gemini-3.1-pro-preview` | Gemini 3.1 Pro Preview |
| `google/gemini-3.1-flash-lite-preview` | Gemini 3.1 Flash Lite Preview |
| `kwaipilot/kat-coder-pro` | KAT Coder Pro |

---

## 5. Verbatim Source Code Snippets

### `open-sse/providers/registry/cline.js`
```javascript
export default {
  id: "cline",
  priority: 80,
  alias: "cl",
  uiAlias: "cl",
  display: {
    name: "Cline",
    icon: "smart_toy",
    color: "#5B9BD5",
    textIcon: "CL",
    website: "https://cline.bot",
    notice: {
      signupUrl: "https://cline.bot",
    },
  },
  category: "oauth",
  authModes: ["oauth"],
  hasOAuth: true,
  transport: {
    baseUrl: "https://api.cline.bot/api/v1/chat/completions",
    headers: {
      "HTTP-Referer": "https://cline.bot",
      "X-Title": "Cline",
    },
    // Non-stream chat completions come back wrapped in {"success":true,"data":{...}}
    quirks: { clineEnvelope: true },
    tokenUrl: "https://api.cline.bot/api/v1/auth/token",
    refreshUrl: "https://api.cline.bot/api/v1/auth/refresh",
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
    { id: "anthropic/claude-opus-4.7", name: "Claude Opus 4.7" },
    { id: "anthropic/claude-sonnet-4.6", name: "Claude Sonnet 4.6" },
    { id: "anthropic/claude-opus-4.6", name: "Claude Opus 4.6" },
    { id: "openai/gpt-5.3-codex", name: "GPT-5.3 Codex" },
    { id: "openai/gpt-5.4", name: "GPT-5.4" },
    { id: "google/gemini-3.1-pro-preview", name: "Gemini 3.1 Pro Preview" },
    { id: "google/gemini-3.1-flash-lite-preview", name: "Gemini 3.1 Flash Lite Preview" },
    { id: "kwaipilot/kat-coder-pro", name: "KAT Coder Pro" },
  ],
  oauth: {
    appBaseUrl: "https://app.cline.bot",
    apiBaseUrl: "https://api.cline.bot",
    authorizeUrl: "https://api.cline.bot/api/v1/auth/authorize",
    tokenExchangeUrl: "https://api.cline.bot/api/v1/auth/token",
    refreshUrl: "https://api.cline.bot/api/v1/auth/refresh",
  },
};
```

### `open-sse/shared/clineAuth.js`
```javascript
import pkg from "../../package.json" with { type: "json" };

const APP_VERSION = pkg.version || "0.0.0";

export function getClineAccessToken(token) {
  if (typeof token !== "string") return "";
  const trimmed = token.trim();
  if (!trimmed) return "";
  if (trimmed.toLowerCase().startsWith("workos:")) return trimmed;
  const isWorkOsJwt = /^eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+/.test(trimmed);
  return isWorkOsJwt ? `workos:${trimmed}` : trimmed;
}

export function getClineAuthorizationHeader(token) {
  const accessToken = getClineAccessToken(token);
  return accessToken ? `Bearer ${accessToken}` : "";
}

export function buildClineHeaders(token, extraHeaders = {}) {
  const authorization = getClineAuthorizationHeader(token);
  const headers = {
    "HTTP-Referer": "https://cline.bot",
    "X-Title": "Cline",
    "User-Agent": `9Router/${APP_VERSION}`,
    "X-PLATFORM": process.platform || "unknown",
    "X-PLATFORM-VERSION": process.version || "unknown",
    "X-CLIENT-TYPE": "9router",
    "X-CLIENT-VERSION": APP_VERSION,
    "X-CORE-VERSION": APP_VERSION,
    "X-IS-MULTIROOT": "false",
    ...extraHeaders,
  };

  if (authorization) {
    headers.Authorization = authorization;
  }

  return headers;
}
```
