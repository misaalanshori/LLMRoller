# DeepSeek (`deepseek`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `deepseek`
- **Priority**: `110`
- **Primary Alias**: `deepseek`
- **Additional Aliases**: `["ds"]`
- **UI Alias**: `ds`
- **Display Name**: DeepSeek
- **Category**: `apikey`
- **Website**: `https://deepseek.com`
- **API Key URL**: `https://platform.deepseek.com/api_keys`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source File**: `open-sse/providers/registry/deepseek.js`

---

## 2. Multi-Endpoint Transport Architecture
DeepSeek declares `transports[]` so `9router` selects the matching native endpoint based on the incoming client's `sourceFormat` to bypass format translation:
1. **OpenAI Format Transport (`format: "openai"`)**:
   - **Endpoint**: `POST https://api.deepseek.com/chat/completions`
   - **Validation Endpoint**: `GET https://api.deepseek.com/models`
   - **Auth**: `Authorization: Bearer <apiKey>`
2. **Claude / Anthropic Format Transport (`format: "claude"`)**:
   - **Endpoint**: `POST https://api.deepseek.com/anthropic/v1/messages`
   - **Auth**: `x-api-key: <apiKey>`
   - **Headers**: `CLAUDE_API_HEADERS` (`Anthropic-Version: 2023-06-01`, `Anthropic-Beta: claude-code-20250219,interleaved-thinking-2025-05-14`)

---

## 3. Protocol Quirks
- **`reasoningInject: { scope: "all" }`**:
  - DeepSeek's API requires assistant messages in multi-turn tool-call conversations to include a `reasoning_content` field when thinking mode is active. `DefaultExecutor.transformRequest` runs `injectReasoningContent(body, "all")` to ensure every assistant turn has `reasoning_content` populated.
- **`quirks.claudeSupportedToolTypes: ["web_search_20250305", "web_search_20260209"]`**:
  - DeepSeek's Anthropic-compatible endpoint (`/anthropic/v1/messages`) rejects client-defined `custom` tools with HTTP 400 (`tools[0]: unknown variant custom, expected web_search_20250305 or web_search_20260209`). Setting this whitelist causes `prepareClaudeRequest()` to strip custom/MCP tools when routing to the Claude-format endpoint while leaving the OpenAI-format endpoint (`/chat/completions`) unaffected.

---

## 4. Supported Models
| Model ID | Display Name | Upstream Model ID |
|---|---|---|
| `deepseek-v4-pro` | DeepSeek V4 Pro | `deepseek-v4-pro` |
| `deepseek-v4-pro-max` | DeepSeek V4 Pro Max | `deepseek-v4-pro` |
| `deepseek-v4-pro-none` | DeepSeek V4 Pro No Thinking | `deepseek-v4-pro` |
| `deepseek-v4.1-flash` | DeepSeek V4.1 Flash | `deepseek-v4.1-flash` |
| `deepseek-v4-flash` | DeepSeek V4 Flash | `deepseek-v4-flash` |
| `deepseek-v4-flash-vision-exp` | DeepSeek V4 Flash Vision (Exp) | `deepseek-v4-flash-vision-exp` |
| `deepseek-chat` | DeepSeek V3.2 Chat | `deepseek-chat` |
| `deepseek-reasoner` | DeepSeek V3.2 Reasoner | `deepseek-reasoner` |

---

## 5. Verbatim Source Code

### `open-sse/providers/registry/deepseek.js`
```javascript
import { CLAUDE_API_HEADERS } from "../shared.js";

export default {
  id: "deepseek",
  priority: 110,
  alias: "deepseek",
  aliases: [
    "ds",
  ],
  uiAlias: "ds",
  display: {
    name: "DeepSeek",
    icon: "bolt",
    color: "#4D6BFE",
    textIcon: "DS",
    website: "https://deepseek.com",
    notice: {
      apiKeyUrl: "https://platform.deepseek.com/api_keys",
    },
  },
  category: "apikey",
  transport: {
    baseUrl: "https://api.deepseek.com/chat/completions",
    validateUrl: "https://api.deepseek.com/models",
    reasoningInject: {
      scope: "all",
    },
    quirks: {
      claudeSupportedToolTypes: ["web_search_20250305", "web_search_20260209"],
    },
  },
  // Multi-endpoint: pick the transport matching client sourceFormat to skip translation.
  transports: [
    {
      format: "openai",
      baseUrl: "https://api.deepseek.com/chat/completions",
      auth: { combined: true, header: "Authorization", scheme: "bearer" },
    },
    {
      format: "claude",
      baseUrl: "https://api.deepseek.com/anthropic/v1/messages",
      headers: { ...CLAUDE_API_HEADERS },
      auth: { combined: true, header: "x-api-key", scheme: "raw" },
    },
  ],
  models: [
    { id: "deepseek-v4-pro", name: "DeepSeek V4 Pro" },
    { id: "deepseek-v4-pro-max", name: "DeepSeek V4 Pro Max", upstreamModelId: "deepseek-v4-pro" },
    { id: "deepseek-v4-pro-none", name: "DeepSeek V4 Pro No Thinking", upstreamModelId: "deepseek-v4-pro" },
    { id: "deepseek-v4.1-flash", name: "DeepSeek V4.1 Flash" },
    { id: "deepseek-v4-flash", name: "DeepSeek V4 Flash" },
    { id: "deepseek-v4-flash-vision-exp", name: "DeepSeek V4 Flash Vision (Exp)" },
    { id: "deepseek-chat", name: "DeepSeek V3.2 Chat" },
    { id: "deepseek-reasoner", name: "DeepSeek V3.2 Reasoner" },
  ],
  features: {
    usage: true,
    usageApikey: true,
  },
};
```
