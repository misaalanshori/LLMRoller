# CodeBuddy CN (`codebuddy-cn`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `codebuddy-cn`
- **Priority**: `90`
- **Alias / UI Alias**: `cbcn`
- **Display Name**: CodeBuddy CN (Tencent Copilot China)
- **Category**: `oauth`
- **Auth Modes**: `["oauth", "apikey"]`
- **Website**: `https://copilot.tencent.com`
- **Executor**: `CodeBuddyExecutor` (`open-sse/executors/codebuddy-cn.js`)
- **Source Files**:
  - `open-sse/providers/registry/codebuddy-cn.js`
  - `open-sse/executors/codebuddy-cn.js`
  - `src/lib/oauth/providers/codebuddy-cn.js`

---

## 2. Endpoints & OAuth Polling Flow
- **Chat Completions Endpoint**: `POST https://copilot.tencent.com/v2/chat/completions`
- **Billing / Quota Usage Endpoint**: `POST https://copilot.tencent.com/v2/billing/meter/get-user-resource`
- **OAuth Device/Browser Polling Flow (`src/lib/oauth/providers/codebuddy-cn.js`)**:
  1. **State Request**: `POST https://copilot.tencent.com/v2/plugin/auth/state?platform=CLI`
     - Headers: `User-Agent: CLI/2.63.2 CodeBuddy/2.63.2`, `X-Requested-With: XMLHttpRequest`, `X-Domain: copilot.tencent.com`, `X-No-Authorization: true`, `X-No-User-Id: true`, `X-Product: SaaS`
     - Body: `"{}"`
     - Returns `{ code: 0, data: { state, authUrl } }`
  2. **Token Polling**: `GET https://copilot.tencent.com/v2/plugin/auth/token?state=<state>` (every 5000ms)
     - Headers: `X-Domain: copilot.tencent.com`, `X-No-Authorization: true`, `X-No-User-Id: true`, `X-No-Enterprise-Id: true`, `X-No-Department-Info: true`, `X-Product: SaaS`, `User-Agent: CLI/2.63.2 CodeBuddy/2.63.2`
     - `data.code === 11217` -> `authorization_pending`
     - `data.code === 0` -> `{ accessToken, refreshToken, tokenType, expiresIn }`
  3. **Token Refresh**: `POST https://copilot.tencent.com/v2/plugin/auth/token/refresh`
     - Implemented in `DefaultExecutor.refreshCodeBuddyToken` with `X-Refresh-Token: <refreshToken>` and `Authorization: Bearer <accessToken>`.

---

## 3. Headers & Request Transformations (`CodeBuddyExecutor`)
- **Static Transport Headers**:
  - `Authorization`: `Bearer <accessToken_or_apiKey>`
  - `User-Agent`: `CLI/2.108.1 CodeBuddy/2.108.1`
  - `X-Product`: `SaaS`
  - `X-IDE-Type`: `CLI`
  - `X-IDE-Name`: `CLI`
  - `x-requested-with`: `XMLHttpRequest`
  - `x-codebuddy-request`: `1`
- **Request Transformations (`CodeBuddyExecutor.transformRequest`)**:
  1. **Forced Streaming (`transformed.stream = true`)**: CodeBuddy rejects non-streaming requests with HTTP 400 (`code: 11101`, `"Non-stream chat request is currently not supported"`).
  2. **Agent System Prompt Sanitization**: Tencent's content filter blocks CLI agent system prompts (such as Claude Code, Cursor, Windsurf, Cline, Aider, or system prompts longer than 2000 characters). Any `system` message matching `text.length > 2000` or `AGENT_PATTERN` is replaced with `"You are a helpful AI assistant that helps with software engineering tasks."`.
  3. **Reasoning Parameters**: Uses `thinkingFormat: "openai"`. If `reasoning_effort` is `"none"` or `"off"`, deletes `reasoning_effort`. If `reasoning_effort` is truthy, sets `transformed.reasoning_summary = "auto"` so CodeBuddy emits reasoning deltas.
- **Error Parsing (`parseError`)**:
  - Maps Tencent error code `6004` (or `/超出频率限制|frequency limit|限额/i`) to HTTP `429` and extracts the reset timestamp (`YYYY-MM-DD HH:MM:SS`, defaulting to `+08:00` if no UTC offset is specified) into `resetsAtMs`.

---

## 4. Supported Models
| Model ID | Display Name |
|---|---|
| `glm-5.2` | GLM-5.2 |
| `glm-5.1` | GLM-5.1 |
| `minimax-m3` | MiniMax-M3 |
| `hy3` | Hy3 |
| `hy4-preview` | Hy4-Preview |
| `glm-5.3` | GLM-5.3 |
| `glm-5.3-flash` | GLM-5.3-Flash |
| `kimi-k3-1` | Kimi-K3 |
| `kimi-k2.8-preview` | Kimi-K2.8-Preview |
| `deepseek-v4-pro` | DeepSeek-V4-Pro |
| `deepseek-v4.1-flash` | DeepSeek-V4.1-Flash |

---

## 5. Verbatim Source Code Snippets

### `open-sse/executors/codebuddy-cn.js`
```javascript
import { DefaultExecutor } from "./default.js";

export class CodeBuddyExecutor extends DefaultExecutor {
  constructor() {
    super("codebuddy-cn");
  }

  transformRequest(model, body, stream, credentials) {
    const transformed = super.transformRequest(model, body, stream, credentials);
    transformed.stream = true;

    const NEUTRAL_PROMPT = "You are a helpful AI assistant that helps with software engineering tasks.";
    const AGENT_PATTERN = /you are claude code|claude.?code.+official.+cli|anthropic.+official.+cli|anxthxropic.+official.+cli|you are (?:cursor|windsurf|cline|aider|continue|copilot|cody)|you are an? (?:ai )?(?:coding |code )?agent|cc_entrypoint\s*=\s*(?:cli|vscode|jetbrains|gui)|claude.?code.+issues|give feedback.+claude.?code|you are .{0,30}(?:powerful )?ai agent|orchestration capabilities|OhMyOpenCode|<agent-identity>|<Role>|<Behavior_Instructions>/i;
    const flatten = (content) =>
      typeof content === "string"
        ? content
        : Array.isArray(content)
          ? content.map((b) => (b && typeof b.text === "string" ? b.text : "")).join("\n")
          : "";
    if (Array.isArray(transformed.messages)) {
      transformed.messages = transformed.messages.map((message) => {
        if (!message || message.role !== "system") return message;
        const text = flatten(message.content);
        if (!text) return message;
        if (text.length > 2000 || AGENT_PATTERN.test(text)) {
          return typeof message.content === "string"
            ? { ...message, content: NEUTRAL_PROMPT }
            : { ...message, content: [{ type: "text", text: NEUTRAL_PROMPT }] };
        }
        return message;
      });
    }

    const eff = transformed.reasoning_effort;
    if (eff === "none" || eff === "off") {
      delete transformed.reasoning_effort;
    } else if (eff) {
      transformed.reasoning_summary = "auto";
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

export default CodeBuddyExecutor;
```
