# Fireworks AI (`fireworks`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `fireworks`
- **Priority**: `50`
- **Alias**: `fireworks`
- **Display Name**: Fireworks AI
- **Category**: `apikey`
- **Auth Type**: `apikey`
- **Service Kinds**: `["llm", "embedding"]`
- **Website**: `https://fireworks.ai`
- **API Key URL**: `https://fireworks.ai/account/api-keys`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source File**: `open-sse/providers/registry/fireworks.js`

---

## 2. Endpoints by Service Kind
- **LLM Chat Completions**: `POST https://api.fireworks.ai/inference/v1/chat/completions`
- **Validation / Models Endpoint**: `GET https://api.fireworks.ai/inference/v1/models`
- **Embeddings Endpoint**: `POST https://api.fireworks.ai/inference/v1/embeddings` (`embeddingConfig.baseUrl`)
- **Wire Format**: `"openai"`

---

## 3. Authentication & Headers
- **Authorization Header**: `Authorization: Bearer <apiKey>`
- **Content-Type**: `application/json`

---

## 4. Supported Models
| Model ID | Display Name | Kind |
|---|---|---|
| `accounts/fireworks/models/deepseek-v3p1` | DeepSeek V3.1 | `llm` |
| `accounts/fireworks/models/llama-v3p3-70b-instruct` | Llama 3.3 70B | `llm` |
| `accounts/fireworks/models/qwen3-235b-a22b` | Qwen3 235B | `llm` |
| `nomic-ai/nomic-embed-text-v1.5` | Nomic Embed Text v1.5 | `embedding` |

---

## 5. Verbatim Source Code

### `open-sse/providers/registry/fireworks.js`
```javascript
export default {
  id: "fireworks",
  priority: 50,
  alias: "fireworks",
  display: {
    name: "Fireworks AI",
    icon: "local_fire_department",
    color: "#7B2EF2",
    textIcon: "FW",
    website: "https://fireworks.ai",
    notice: {
      apiKeyUrl: "https://fireworks.ai/account/api-keys",
    },
  },
  category: "apikey",
  authType: "apikey",
  transport: {
    baseUrl: "https://api.fireworks.ai/inference/v1/chat/completions",
    validateUrl: "https://api.fireworks.ai/inference/v1/models",
  },
  models: [
    { id: "accounts/fireworks/models/deepseek-v3p1", name: "DeepSeek V3.1" },
    { id: "accounts/fireworks/models/llama-v3p3-70b-instruct", name: "Llama 3.3 70B" },
    { id: "accounts/fireworks/models/qwen3-235b-a22b", name: "Qwen3 235B" },
    { id: "nomic-ai/nomic-embed-text-v1.5", name: "Nomic Embed Text v1.5", kind: "embedding" },
  ],
  serviceKinds: ["llm", "embedding"],
  embeddingConfig: { baseUrl: "https://api.fireworks.ai/inference/v1/embeddings" },
};
```
