# Kilo Code (`kilocode`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `kilocode`
- **Priority**: `70`
- **Alias / UI Alias**: `kc`
- **Display Name**: Kilo Code
- **Category**: `oauth`
- **Website**: `https://kilocode.ai`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source Files**:
  - `open-sse/providers/registry/kilocode.js`
  - `open-sse/executors/default.js` (`kilocodeOrg` header hook)
  - `src/lib/oauth/providers/kilocode.js`

---

## 2. Endpoints & Device Code OAuth Flow
- **Chat Completions Endpoint**: `POST https://api.kilo.ai/api/openrouter/chat/completions`
- **Dynamic Models Fetcher**: `GET https://api.kilo.ai/api/gateway/models` (`type: "openrouter-free"`, `passthroughModels: true`)
- **OAuth Device Code Flow (`src/lib/oauth/providers/kilocode.js`)**:
  1. **Initiate**: `POST https://api.kilo.ai/api/device-auth/codes` (`Content-Type: application/json`) -> returns `{ code, verificationUrl, expiresIn }`.
  2. **Poll**: `GET https://api.kilo.ai/api/device-auth/codes/<code_id>` (every 3s; `202` = `authorization_pending`, `403` = `access_denied`, `410` = `expired_token`, `200` with `data.status === "approved"` = `{ token, userEmail }`).
  3. **Profile Lookup**: `GET https://api.kilo.ai/api/profile` with `Authorization: Bearer <token>` -> extracts `profile.organizations?.[0]?.id` and stores it as `providerSpecificData.orgId`.

---

## 3. Authentication & Organization Header Hook (`kilocodeOrg`)
- **Authorization Header**: `Authorization: Bearer <accessToken>`
- **Organization Header (`HEADER_HOOKS.kilocodeOrg`)**:
  ```javascript
  kilocodeOrg: (h, c) => {
    if (c.providerSpecificData?.orgId) h["X-Kilocode-OrganizationID"] = c.providerSpecificData.orgId;
  }
  ```

---

## 4. Seed Models
| Model ID | Display Name |
|---|---|
| `anthropic/claude-sonnet-4-20250514` | Claude Sonnet 4 |
| `anthropic/claude-opus-4-20250514` | Claude Opus 4 |
| `google/gemini-2.5-pro` | Gemini 2.5 Pro |
| `google/gemini-2.5-flash` | Gemini 2.5 Flash |
| `openai/gpt-4.1` | GPT-4.1 |
| `openai/o3` | o3 |
| `deepseek/deepseek-chat` | DeepSeek Chat |
| `deepseek/deepseek-reasoner` | DeepSeek Reasoner |

---

## 5. Verbatim Source Code Snippets

### `open-sse/providers/registry/kilocode.js`
```javascript
export default {
  id: "kilocode",
  priority: 70,
  alias: "kc",
  uiAlias: "kc",
  display: {
    name: "Kilo Code",
    icon: "code",
    color: "#FF6B35",
    textIcon: "KC",
    website: "https://kilocode.ai",
    notice: {
      signupUrl: "https://kilocode.ai",
    },
  },
  category: "oauth",
  transport: {
    baseUrl: "https://api.kilo.ai/api/openrouter/chat/completions",
    headers: {},
    auth: {
      combined: true,
      header: "Authorization",
      scheme: "bearer",
      hooks: [
        "kilocodeOrg",
      ],
    },
  },
  models: [
    { id: "anthropic/claude-sonnet-4-20250514", name: "Claude Sonnet 4" },
    { id: "anthropic/claude-opus-4-20250514", name: "Claude Opus 4" },
    { id: "google/gemini-2.5-pro", name: "Gemini 2.5 Pro" },
    { id: "google/gemini-2.5-flash", name: "Gemini 2.5 Flash" },
    { id: "openai/gpt-4.1", name: "GPT-4.1" },
    { id: "openai/o3", name: "o3" },
    { id: "deepseek/deepseek-chat", name: "DeepSeek Chat" },
    { id: "deepseek/deepseek-reasoner", name: "DeepSeek Reasoner" },
  ],
  modelsFetcher: { url: "https://api.kilo.ai/api/gateway/models", type: "openrouter-free" },
  passthroughModels: true,
  oauth: {
    apiBaseUrl: "https://api.kilo.ai",
    initiateUrl: "https://api.kilo.ai/api/device-auth/codes",
    pollUrlBase: "https://api.kilo.ai/api/device-auth/codes",
  },
};
```
