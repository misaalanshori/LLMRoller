# Mistral (`mistral`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `mistral`
- **Priority**: `80`
- **Alias**: `mistral`
- **Display Name**: Mistral
- **Category**: `apikey`
- **Service Kinds**: `["llm", "imageToText", "embedding"]`
- **Website**: `https://mistral.ai`
- **API Key URL**: `https://console.mistral.ai/api-keys`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source File**: `open-sse/providers/registry/mistral.js`

---

## 2. Endpoints & Quirks
- **Chat Completions Endpoint**: `POST https://api.mistral.ai/v1/chat/completions`
- **Validation / Models Endpoint**: `GET https://api.mistral.ai/v1/models`
- **Embeddings Endpoint**: `POST https://api.mistral.ai/v1/embeddings`
- **Authentication**: `Authorization: Bearer <apiKey>`
- **Protocol Quirks**:
  - `dropClientMetadata: true`: Strips client `metadata` and `user` fields before sending the request to Mistral's API, as Mistral's schema validator rejects unknown top-level metadata fields.

---

## 3. Supported Models
| Model ID | Display Name | Kind |
|---|---|---|
| `mistral-large-latest` | Mistral Large 3 | `llm` |
| `codestral-latest` | Codestral | `llm` |
| `mistral-medium-latest` | Mistral Medium 3 | `llm` |
| `mistral-embed` | Mistral Embed | `embedding` |

---

## 4. Verbatim Source Code

### `open-sse/providers/registry/mistral.js`
```javascript
export default {
  id: "mistral",
  priority: 80,
  alias: "mistral",
  display: {
    name: "Mistral",
    icon: "air",
    color: "#FF7000",
    textIcon: "MI",
    website: "https://mistral.ai",
    notice: {
      apiKeyUrl: "https://console.mistral.ai/api-keys",
    },
  },
  category: "apikey",
  transport: {
    baseUrl: "https://api.mistral.ai/v1/chat/completions",
    validateUrl: "https://api.mistral.ai/v1/models",
    quirks: {
      dropClientMetadata: true,
    },
  },
  models: [
    { id: "mistral-large-latest", name: "Mistral Large 3" },
    { id: "codestral-latest", name: "Codestral" },
    { id: "mistral-medium-latest", name: "Mistral Medium 3" },
    { id: "mistral-embed", name: "Mistral Embed", kind: "embedding" },
  ],
  serviceKinds: ["llm","imageToText","embedding"],
  embeddingConfig: { baseUrl: "https://api.mistral.ai/v1/embeddings", authType: "apikey", authHeader: "bearer" },
};
```
