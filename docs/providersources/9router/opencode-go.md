# OpenCode Go (`opencode-go`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `opencode-go`
- **Priority**: `210`
- **Primary Alias**: `opencode-go`
- **Additional Aliases**: `["ocg"]`
- **UI Alias**: `ocg`
- **Display Name**: OpenCode Go
- **Category**: `apikey`
- **Website**: `https://opencode.ai/auth`
- **Executor**: `OpenCodeGoExecutor` (`open-sse/executors/opencode-go.js`)
- **Source Files**:
  - `open-sse/providers/registry/opencode-go.js`
  - `open-sse/executors/opencode-go.js`

---

## 2. Tri-Endpoint Multi-Format Transport
`opencode-go` declares three format-specific lanes in `transports[]`, guarded per-model by `supportedFormats`:
1. **OpenAI Chat Completions Lane (`format: "openai"`)**:
   - **Endpoint**: `POST https://opencode.ai/zen/go/v1/chat/completions`
   - **Auth**: `Authorization: Bearer <apiKey>`
2. **Claude / Anthropic Messages Lane (`format: "claude"`)**:
   - **Endpoint**: `POST https://opencode.ai/zen/go/v1/messages`
   - **Auth**: `x-api-key: <apiKey>` + `anthropic-version: 2023-06-01`
3. **OpenAI Responses Lane (`format: "openai-responses"`, `thinkingFormat: "openai-responses"`)**:
   - **Endpoint**: `POST https://opencode.ai/zen/go/v1/responses`
   - **Auth**: `Authorization: Bearer <apiKey>`
4. **Usage & Models Endpoints**:
   - `usage.url`: `https://opencode.ai/zen/go/v1/usage`
   - `modelsFetcher.url`: `https://opencode.ai/zen/go/v1/models` (`type: "opencode-go"`, `passthroughModels: true`)

---

## 3. Session Header & Responses Sanitization (`OpenCodeGoExecutor`)
- **`x-opencode-session` Header**:
  - Preserves incoming `x-opencode-session` if present, or derives a deterministic `ses_<32hex>` session ID via `SHA256("opencode-go\0" + clientTool + "\0" + resolvedSessionId).slice(0, 32)`.
- **Responses API Sanitization (`transformRequest` when `isResponsesModel(model) === true`)**:
  - Forces `out.stream = true` and `out.store = false`.
  - Converts `max_tokens` / `max_completion_tokens` -> `max_output_tokens`.
  - Converts `reasoning_effort` -> `reasoning: { effort, summary: "auto" }`.
  - Flattens tools (`normalizeResponsesTools`) and strips prior-turn `type === "reasoning"` items and `encrypted_content` (`sanitizeResponsesItems`) so Muse Spark contributor models do not fail with HTTP 400 `"reasoning encrypted_content was not issued to this caller"`.

---

## 4. Supported Models
| Model Group | Model IDs | Supported Formats |
|---|---|---|
| **OpenAI + Claude + Responses** | `deepseek-v4-pro`, `deepseek-v4-flash`, `deepseek-v4-flash-vision-exp`, `deepseek-v4.1-flash` | `["openai", "claude", "openai-responses"]` |
| **OpenAI + Claude** | `minimax-m3`, `minimax-m2.7`, `minimax-m2.5`, `space-bunny-free`, `qwen3.8-max`, `qwen3.8-flash`, `qwen3.7-max`, `qwen3.7-plus`, `qwen3.6-plus`, `qwen3.5-plus` | `["openai", "claude"]` |
| **OpenAI Only** | `deepseek-flash`, `glm-5.3-flash`, `glm-5.3`, `glm-5.2`, `glm-5.1`, `glm-5`, `kimi-k2.7-code`, `kimi-k2.6`, `kimi-k2.5`, `kimi-k3`, `longcat-2.0`, `mimo-v2.6-flash`, `mimo-v2.6-pro`, `mimo-v2.5`, `mimo-v2.5-pro`, `mimo-v2-pro`, `mimo-v2-omni`, `hy4-preview`, `hy3`, `hy3-preview`, `omen-alpha` | `["openai"]` |
| **Responses Only** | `grok-4.7`, `grok-4.6`, `grok-4.5`, `gpt-5.6-luna`, `gpt-6-luna`, `muse-spark-1.2-contributor`, `muse-spark-1.3-contributor` | `["openai-responses"]` |
| **Claude Only** | `claude-haiku-5-5` | `["claude"]` |

---

## 5. Verbatim Source Code Snippets

### `open-sse/executors/opencode-go.js` (Excerpt)
```javascript
function translatedSession(sessionId, clientTool) {
  const digest = crypto
    .createHash("sha256")
    .update(`opencode-go\0${clientTool || "generic"}\0${sessionId}`)
    .digest("hex")
    .slice(0, 32);
  return `ses_${digest}`;
}

export class OpenCodeGoExecutor extends DefaultExecutor {
  constructor() {
    super("opencode-go");
  }

  buildUrl(model, stream, urlIndex = 0, credentials = null) {
    if (isResponsesModel(model)) return RESPONSES_BASE_URL;
    return super.buildUrl(model, stream, urlIndex, credentials);
  }
```
