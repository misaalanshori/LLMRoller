# Nebius AI (`nebius`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `nebius`
- **Priority**: `70`
- **Alias**: `nebius`
- **Display Name**: Nebius AI
- **Category**: `apikey`
- **Auth Type**: `apikey`
- **Service Kinds**: `["llm", "embedding"]`
- **Website**: `https://nebius.com`
- **API Key URL**: `https://studio.nebius.com/settings/api-keys`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source File**: `open-sse/providers/registry/nebius.js`

---

## 2. Endpoints by Service Kind
- **LLM Chat Completions**: `POST https://api.studio.nebius.ai/v1/chat/completions`
- **Validation / Models Endpoint**: `GET https://api.studio.nebius.ai/v1/models`
- **Embeddings Endpoint**: `POST https://api.tokenfactory.nebius.com/v1/embeddings` (`embeddingConfig.baseUrl`)
- **Authentication**: `Authorization: Bearer <apiKey>`

---

## 3. Supported Models
| Model ID | Display Name | Kind |
|---|---|---|
| `meta-llama/Llama-3.3-70B-Instruct` | Llama 3.3 70B Instruct | `llm` |
| `Qwen/Qwen3-Embedding-8B` | Qwen3 Embedding 8B | `embedding` |

---

## 4. Verbatim Source Code

### `open-sse/providers/registry/nebius.js`
```javascript
export default {
  id: "nebius",
  priority: 70,
  alias: "nebius",
  display: {
    name: "Nebius AI",
    icon: "cloud",
    color: "#6C5CE7",
    textIcon: "NB",
    website: "https://nebius.com",
    notice: {
      apiKeyUrl: "https://studio.nebius.com/settings/api-keys",
    },
  },
  category: "apikey",
  authType: "apikey",
  transport: {
    baseUrl: "https://api.studio.nebius.ai/v1/chat/completions",
    validateUrl: "https://api.studio.nebius.ai/v1/models",
  },
  models: [
    { id: "meta-llama/Llama-3.3-70B-Instruct", name: "Llama 3.3 70B Instruct" },
    { id: "Qwen/Qwen3-Embedding-8B", name: "Qwen3 Embedding 8B", kind: "embedding" },
  ],
  serviceKinds: ["llm", "embedding"],
  embeddingConfig: { baseUrl: "https://api.tokenfactory.nebius.com/v1/embeddings" },
};
```
