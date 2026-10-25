# MiniMax Code Global (`minimax-code-global`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `minimax-code-global`
- **Priority**: `80`
- **Alias**: `mmg`
- **Display Name**: MiniMax Code (Global)
- **Category**: `oauth`
- **Auth Modes**: `["oauth"]`
- **Website**: `https://agent.minimax.io`
- **Executor**: `MinimaxCodeExecutor` (`open-sse/executors/minimax-code.js`)
- **Source Files**:
  - `open-sse/providers/registry/minimax-code-global.js`
  - `open-sse/executors/minimax-code.js`
  - `open-sse/executors/default.js` (`minimaxHeaders` hook)
  - `src/lib/oauth/providers/minimax-code-global.js`
  - `src/lib/oauth/providers/minimax-code-shared.js`

---

## 2. Endpoints & OAuth 2.0 PKCE Device Code Flow
- **Messages Endpoint**: `POST https://agent.minimax.io/mavis/api/v1/llm/v1/messages` (`format: "claude"`, `thinkingFormat: "claude-adaptive"`)
- **Models Fetcher**: `GET https://agent.minimax.io/mavis/api/v1/models?region=en&buildEnv=prod` (`type: "minimax-code"`)
- **OAuth 2.0 Device Flow + PKCE (`S256`) (`minimax-code-shared.js`)**:
  - `clientId`: `"mcode-public"`
  - `scope`: `"agent.default"`
  - `audience`: `"agent-backend"`
  - `deviceUrl`: `POST https://account.minimax.io/oauth2/device/code` (sends `client_id`, `scope`, `audience`, `code_challenge`, `code_challenge_method=S256`)
  - `tokenUrl`: `POST https://account.minimax.io/oauth2/token` (sends `grant_type=urn:ietf:params:oauth:grant-type:device_code`, `device_code` or `user_code` if `device_code` was omitted by upstream, `client_id`, `code_verifier`)
  - `refreshLeadMs`: `600000` (10 minutes; single-use refresh tokens handled via `refreshMiniMaxCodeToken`).

---

## 3. Headers & Quota Normalization (`minimaxHeaders` & `MinimaxCodeExecutor`)
- **Static & Hook Headers**:
  - `Authorization`: `Bearer <accessToken>`
  - `anthropic-version`: `"2023-06-01"` (`anthropicVersion: true`)
  - `User-Agent`: `"MiniMaxAgent"`
  - `X-Mavis-Agent-Id`: `"main"`
  - `x-api-key`: `"sk-xxx"` (injected by `HEADER_HOOKS.minimaxHeaders` alongside `Authorization: Bearer`)
  - `X-Mavis-Timezone-Offset`: `String(new Date().getTimezoneOffset() * -60)`
  - `X-Mavis-Session-Id`: `<randomUUID()>`
- **Quota Error Normalization (`normalizeQuotaResponse`)**:
  - Rewrites HTTP `402` and `403` responses whose body matches `/insufficient|balance|credit|quota|exhaust|limit|余额|积分|额度|不足|用完|上限/i` into HTTP `429` (`rate_limit_error`) so `chatCore` fails over to the next account rather than attempting a token refresh.

---

## 4. Supported Models
| Model ID | Display Name |
|---|---|
| `MiniMax-M3.1-Flash-Preview` | MiniMax M3.1 Flash Preview |
| `MiniMax-M3` | MiniMax M3 |
| `MiniMax-M2.7` | MiniMax M2.7 |
| `MiniMax-M2.7-highspeed` | MiniMax M2.7 Highspeed |

---

## 5. Verbatim Source Code

### `open-sse/providers/registry/minimax-code-global.js`
```javascript
export default {
  id: "minimax-code-global",
  priority: 80,
  alias: "mmg",
  display: {
    name: "MiniMax Code (Global)",
    icon: "smart_toy",
    color: "#FF4D4F",
    textIcon: "MM",
    website: "https://agent.minimax.io",
  },
  category: "oauth",
  authModes: ["oauth"],
  hasOAuth: true,
  features: { usage: true },
  modelsFetcher: { url: "https://agent.minimax.io/mavis/api/v1/models?region=en&buildEnv=prod", type: "minimax-code" },
  transport: {
    baseUrl: "https://agent.minimax.io/mavis/api/v1/llm/v1/messages",
    format: "claude",
    thinkingFormat: "claude-adaptive",
    headers: {
      "User-Agent": "MiniMaxAgent",
      "X-Mavis-Agent-Id": "main",
    },
    auth: {
      combined: true,
      header: "Authorization",
      scheme: "bearer",
      anthropicVersion: true,
      hooks: ["minimaxHeaders"],
    },
  },
  oauth: {
    clientId: "mcode-public",
    deviceUrl: "https://account.minimax.io/oauth2/device/code",
    tokenUrl: "https://account.minimax.io/oauth2/token",
    scope: "agent.default",
    audience: "agent-backend",
    refreshLeadMs: 10 * 60 * 1000,
  },
  models: [
    { id: "MiniMax-M3.1-Flash-Preview", name: "MiniMax M3.1 Flash Preview" },
    { id: "MiniMax-M3", name: "MiniMax M3" },
    { id: "MiniMax-M2.7", name: "MiniMax M2.7" },
    { id: "MiniMax-M2.7-highspeed", name: "MiniMax M2.7 Highspeed" },
  ],
};
```
