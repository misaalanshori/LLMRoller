# Dahl Inference (`dahl`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `dahl`
- **Priority**: `120`
- **Primary Alias**: `dahl`
- **Additional Aliases**: `["dahl-inference"]`
- **UI Alias**: `dahl`
- **Display Name**: Dahl Inference
- **Category**: `apikey`
- **Auth Type**: `apikey`
- **Website**: `https://dahl.global`
- **API Key URL**: `https://dahl.global/dashboard`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source File**: `open-sse/providers/registry/dahl.js`

---

## 2. Endpoints & Transport
- **Chat Completions Endpoint**: `POST https://inference.dahl.global/v1/chat/completions`
- **Validation / Models Endpoint**: `GET https://inference.dahl.global/v1/models`
- **Dynamic Models Fetcher**: `{ url: "https://inference.dahl.global/v1/models", type: "openai" }` (public catalogue, works without auth)
- **Passthrough Models**: `true`
- **Wire Format**: `"openai"`

---

## 3. Authentication & Headers
- **Authentication**: `Authorization: Bearer <apiKey>`
- **Content-Type**: `application/json`

---

## 4. Seed Models
| Model ID | Display Name |
|---|---|
| `zai-org/GLM-5.3-Flash` | GLM-5.3 Flash |
| `deepseek-ai/DeepSeek-V4-Flash-0731` | DeepSeek V4 Flash 0731 |
| `MiniMaxAI/MiniMax-M2.7` | MiniMax M2.7 |

---

## 5. Verbatim Source Code

### `open-sse/providers/registry/dahl.js`
```javascript
export default {
  id: "dahl",
  priority: 120,
  alias: "dahl",
  aliases: [
    "dahl-inference",
  ],
  uiAlias: "dahl",
  display: {
    name: "Dahl Inference",
    icon: "hub",
    color: "#1E40AF",
    textIcon: "DH",
    website: "https://dahl.global",
    notice: {
      text: "OpenAI-compatible Gonka inference node. Small, fixed catalogue (GLM-5.3-Flash, DeepSeek-V4-Flash, MiniMax-M2.7) at a flat per-token rate.",
      apiKeyUrl: "https://dahl.global/dashboard",
    },
  },
  category: "apikey",
  authType: "apikey",
  transport: {
    baseUrl: "https://inference.dahl.global/v1/chat/completions",
    validateUrl: "https://inference.dahl.global/v1/models",
  },
  models: [
    { id: "zai-org/GLM-5.3-Flash", name: "GLM-5.3 Flash" },
    { id: "deepseek-ai/DeepSeek-V4-Flash-0731", name: "DeepSeek V4 Flash 0731" },
    { id: "MiniMaxAI/MiniMax-M2.7", name: "MiniMax M2.7" },
  ],
  modelsFetcher: { url: "https://inference.dahl.global/v1/models", type: "openai" },
  passthroughModels: true,
};
```
