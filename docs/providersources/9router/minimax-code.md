# MiniMax Code China (`minimax-code`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `minimax-code`
- **Priority**: `80`
- **Alias**: `mm`
- **Display Name**: MiniMax Code
- **Category**: `oauth`
- **Auth Modes**: `["oauth"]`
- **Website**: `https://agent.minimax.cn`
- **Executor**: `MinimaxCodeExecutor` (`open-sse/executors/minimax-code.js`)
- **Source Files**:
  - `open-sse/providers/registry/minimax-code.js`
  - `open-sse/executors/minimax-code.js`
  - `open-sse/executors/default.js` (`minimaxHeaders` hook)
  - `src/lib/oauth/providers/minimax-code.js`
  - `src/lib/oauth/providers/minimax-code-shared.js`

---

## 2. Endpoints & OAuth 2.0 PKCE Device Code Flow
- **Messages Endpoint**: `POST https://agent.minimax.cn/mavis/api/v1/llm/v1/messages` (`format: "claude"`, `thinkingFormat: "claude-adaptive"`)
- **Models Fetcher**: `GET https://agent.minimax.cn/mavis/api/v1/models?region=cn&buildEnv=prod` (`type: "minimax-code"`)
- **OAuth 2.0 Device Flow + PKCE (`S256`)**:
  - `clientId`: `"mcode-public"`
  - `scope`: `"agent.default"`
  - `audience`: `"agent-backend"`
  - `deviceUrl`: `POST https://account.minimax.cn/oauth2/device/code`
  - `tokenUrl`: `POST https://account.minimax.cn/oauth2/token`
  - `refreshLeadMs`: `600000` (10 minutes)

---

## 3. Headers & Quota Normalization (`MinimaxCodeExecutor`)
- **Headers**:
  - `Authorization`: `Bearer <accessToken>`
  - `anthropic-version`: `"2023-06-01"`
  - `User-Agent`: `"MiniMaxAgent"`
  - `X-Mavis-Agent-Id`: `"main"`
  - `x-api-key`: `"sk-xxx"` (injected by `HEADER_HOOKS.minimaxHeaders`)
  - `X-Mavis-Timezone-Offset`: `String(new Date().getTimezoneOffset() * -60)`
  - `X-Mavis-Session-Id`: `<randomUUID()>`
- **Quota Error Normalization (`normalizeQuotaResponse`)**:
  - Converts HTTP `402` and `403` responses matching `QUOTA_RE` (`/insufficient|balance|credit|quota|exhaust|limit|余额|积分|额度|不足|用完|上限/i`) into HTTP `429` (`rate_limit_error`).

---

## 4. Supported Models
| Model ID | Display Name |
|---|---|
| `MiniMax-M3.1-Flash-Preview` | MiniMax M3.1 Flash Preview |
| `MiniMax-M3` | MiniMax M3 |
| `MiniMax-M2.7` | MiniMax M2.7 |
| `MiniMax-M2.7-highspeed` | MiniMax M2.7 Highspeed |

---

## 5. Verbatim Source Code Snippets

### `open-sse/executors/minimax-code.js`
```javascript
import { DefaultExecutor } from "./default.js";
import { refreshMiniMaxCodeToken } from "../services/tokenRefresh.js";

const QUOTA_RE = /insufficient|balance|credit|quota|exhaust|limit|余额|积分|额度|不足|用完|上限/i;

export async function normalizeQuotaResponse(response) {
  const status = response?.status;
  if (!response || (status !== 402 && status !== 403 && status !== 429)) return response;

  const text = await response.text().catch(() => "");
  if (status === 429 || !QUOTA_RE.test(text)) {
    const headers = new Headers(response.headers);
    headers.delete("content-length");
    headers.delete("content-encoding");
    return new Response(text, { status, statusText: response.statusText, headers });
  }

  let msg = text.trim();
  try {
    const v = JSON.parse(text);
    msg = v?.error?.message ?? v?.message ?? v?.msg ?? v?.base_resp?.status_msg ?? msg;
  } catch { /* keep raw text */ }
  const headers = new Headers(response.headers);
  headers.delete("content-length");
  headers.delete("content-encoding");
  headers.set("content-type", "application/json");
  return new Response(
    JSON.stringify({ type: "error", error: { type: "rate_limit_error", message: `usage limit reached: ${msg}` } }),
    { status: 429, statusText: "Too Many Requests", headers }
  );
}

export class MinimaxCodeExecutor extends DefaultExecutor {
  async execute(args) {
    const result = await super.execute(args);
    if (!result?.response) return result;
    return { ...result, response: await normalizeQuotaResponse(result.response) };
  }

  async refreshCredentials(credentials, log) {
    if (!credentials?.refreshToken) return null;
    return refreshMiniMaxCodeToken(this.provider, credentials.refreshToken, log);
  }
}
```
