# LLM7 (`llm7`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `llm7`
- **Primary Alias**: `llm7`
- **Additional Aliases**: `["llm-7"]`
- **UI Alias**: `llm7`
- **Display Name**: LLM7
- **Category**: `apikey`
- **Auth Type**: `apikey` (`authModes: ["apikey"]`)
- **Website**: `https://llm7.io`
- **API Key URL**: `https://llm7.io`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source File**: `open-sse/providers/registry/llm7.js`

---

## 2. Endpoints & Transport
- **Chat Completions Endpoint**: `POST https://api.llm7.io/v1/chat/completions`
- **Validation / Models Endpoint**: `GET https://api.llm7.io/v1/models`
- **Passthrough Models**: `true`
- **Wire Format**: `"openai"`
- **Authentication**: `Authorization: Bearer <apiKey>`

---

## 3. Supported Models
| Model ID | Display Name | Context Length |
|---|---|---|
| `gpt-5.5` | GPT-5.5 (LLM7) | 1,050,000 |
| `claude-opus-5` | Claude Opus 5 (LLM7) | 1,000,000 |
| `deepseek-v4-flash` | DeepSeek V4 Flash (LLM7) | 1,000,000 |
| `grok-4.5` | Grok 4.5 (LLM7) | 500,000 |
| `kimi-k3` | Kimi K3 (LLM7) | 1,000,000 |

---

## 4. Verbatim Source Code

### `open-sse/providers/registry/llm7.js`
```javascript
export default {
  id: "llm7",
  alias: "llm7",
  aliases: [
    "llm-7",
  ],
  uiAlias: "llm7",
  display: {
    name: "LLM7",
    icon: "pool",
    color: "#7C3AED",
    textIcon: "L7",
    website: "https://llm7.io",
    notice: {
      apiKeyUrl: "https://llm7.io",
    },
  },
  category: "apikey",
  authType: "apikey",
  authModes: [
    "apikey",
  ],
  transport: {
    baseUrl: "https://api.llm7.io/v1/chat/completions",
    validateUrl: "https://api.llm7.io/v1/models",
  },
  models: [
    { id: "gpt-5.5", name: "GPT-5.5 (LLM7)", contextLength: 1050000 },
    { id: "claude-opus-5", name: "Claude Opus 5 (LLM7)", contextLength: 1000000 },
    { id: "deepseek-v4-flash", name: "DeepSeek V4 Flash (LLM7)", contextLength: 1000000 },
    { id: "grok-4.5", name: "Grok 4.5 (LLM7)", contextLength: 500,000 },
    { id: "kimi-k3", name: "Kimi K3 (LLM7)", contextLength: 1000000 },
  ],
  passthroughModels: true,
};
```
