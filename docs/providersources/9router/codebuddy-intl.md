# CodeBuddy International (`codebuddy-intl`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `codebuddy-intl`
- **Priority**: `90`
- **Alias / UI Alias**: `cbai`
- **Display Name**: CodeBuddy (International)
- **Category**: `oauth`
- **Auth Modes**: `["oauth", "apikey"]`
- **Website**: `https://www.codebuddy.ai`
- **Executor**: `CodeBuddyIntlExecutor` (`open-sse/executors/codebuddy-intl.js`)
- **Source Files**:
  - `open-sse/providers/registry/codebuddy-intl.js`
  - `open-sse/executors/codebuddy-intl.js`
  - `src/lib/oauth/providers/codebuddy-intl.js`

---

## 2. Endpoints & OAuth Flow
- **Chat Completions Endpoint**: `POST https://www.codebuddy.ai/v2/chat/completions`
- **Billing / Quota Endpoint**: `POST https://www.codebuddy.ai/v2/billing/meter/get-user-resource`
- **OAuth Polling Endpoints (`platform=ide`)**:
  - **State Request**: `POST https://www.codebuddy.ai/v2/plugin/auth/state?platform=ide`
    - Headers: `User-Agent: IDE/2.63.2 CodeBuddy/2.63.2`, `X-Domain: www.codebuddy.ai`, `X-Requested-With: XMLHttpRequest`, `X-No-Authorization: true`, `X-No-User-Id: true`, `X-Product: SaaS`
  - **Token Poll**: `GET https://www.codebuddy.ai/v2/plugin/auth/token?state=<state>`
  - **Token Refresh**: `POST https://www.codebuddy.ai/v2/plugin/auth/token/refresh`

---

## 3. Headers & Request Transformations (`CodeBuddyIntlExecutor`)
- **Static Transport Headers**:
  - `Authorization`: `Bearer <accessToken_or_apiKey>`
  - `User-Agent`: `IDE/2.108.1 CodeBuddy/2.108.1`
  - `X-Product`: `SaaS`
  - `X-IDE-Type`: `IDE`
  - `X-IDE-Name`: `IDE`
  - `x-requested-with`: `XMLHttpRequest`
  - `x-codebuddy-request`: `1`
- **Request Transformations (`CodeBuddyIntlExecutor.transformRequest`)**:
  1. **Forced Streaming**: Sets `transformed.stream = true`.
  2. **Reasoning Parameters**: Deletes `reasoning_effort` when `"none"` or `"off"`; sets `transformed.reasoning_summary = "auto"` when `reasoning_effort` is present.
  3. **Fixed System Prompt & Typed User Content Blocks**:
     - Unlike `codebuddy-cn`, `codebuddy.ai` rejects plain OpenAI requests (`code: 11101 invalid request`) unless there is a leading system message `"You are CodeBuddy Code."` and user message `content` is formatted as an array of typed blocks (`[{ type: "text", text: message.content }]`).
     - `CodeBuddyIntlExecutor` strips incoming `system` and `developer` messages, prepends `{ role: "system", content: "You are CodeBuddy Code." }`, and converts string `user` message content into `[{ type: "text", text: message.content }]`.

---

## 4. Supported Models
| Model ID | Display Name |
|---|---|
| `glm-5.2` | GLM-5.2 |
| `glm-5.1` | GLM-5.1 |
| `glm-5.0` | GLM-5.0 |
| `glm-5.0-turbo` | GLM-5.0-Turbo |
| `glm-5v-turbo` | GLM-5v-Turbo |
| `glm-4.7` | GLM-4.7 |
| `minimax-m3` | MiniMax-M3 |
| `minimax-m2.7` | MiniMax-M2.7 |
| `kimi-k2.7` | Kimi-K2.7-Code |
| `kimi-k2.6` | Kimi-K2.6 |
| `kimi-k2.5` | Kimi-K2.5 |
| `hy3-preview` | Hy3 Preview |
| `deepseek-v4-pro` | DeepSeek-V4-Pro |
| `deepseek-v4.1-flash` | DeepSeek-V4.1-Flash |
| `deepseek-v3-2-volc` | DeepSeek-V3.2 |

---

## 5. Verbatim Source Code Snippets

### `open-sse/executors/codebuddy-intl.js`
```javascript
import { DefaultExecutor } from "./default.js";

export class CodeBuddyIntlExecutor extends DefaultExecutor {
  constructor() {
    super("codebuddy-intl");
  }

  transformRequest(model, body, stream, credentials) {
    const transformed = super.transformRequest(model, body, stream, credentials);
    transformed.stream = true;

    const eff = transformed.reasoning_effort;
    if (eff === "none" || eff === "off") {
      delete transformed.reasoning_effort;
    } else if (eff) {
      transformed.reasoning_summary = "auto";
    }

    // CodeBuddy rejects plain OpenAI shape (11101 invalid request): needs a
    // leading system prompt + user content as typed blocks, not a bare string.
    const source = Array.isArray(transformed.messages) ? transformed.messages : [];
    transformed.messages = [{ role: "system", content: "You are CodeBuddy Code." }];
    for (const message of source) {
      if (!message || typeof message !== "object" || ["system", "developer"].includes(message.role)) continue;
      if (message.role === "user" && typeof message.content === "string") {
        transformed.messages.push({ ...message, content: [{ type: "text", text: message.content }] });
      } else {
        transformed.messages.push({ ...message });
      }
    }

    return transformed;
  }

  parseError(response, bodyText) {
    if (bodyText) {
      try {
        const data = JSON.parse(bodyText);
        const msg = data?.msg || data?.message || data?.error?.message || "";
        if (data?.code === 6004 || /超出频率限制|frequency limit|限额/i.test(msg)) {
          let resetsAtMs = null;
          const match = msg.match(/(\d{4}-\d{2}-\d{2})\s+(\d{2}:\d{2}:\d{2})(?:\s*UTC\+?([0-9:]+))?/i);
          if (match) {
            const dp = match[1];
            const tp = match[2];
            const tz = match[3]
              ? (match[3].includes(":") ? (match[3].startsWith("+") ? match[3] : `+${match[3]}`) : `+${match[3].padStart(2, "0")}:00`)
              : "+08:00";
            const dt = new Date(`${dp}T${tp}${tz}`);
            if (!isNaN(dt.getTime())) resetsAtMs = dt.getTime();
          }
          return {
            status: 429,
            message: msg || "CodeBuddy frequency limit (6004)",
            resetsAtMs,
          };
        }
      } catch {}
    }
    return super.parseError(response, bodyText);
  }
}

export default CodeBuddyIntlExecutor;
```
