# Jina AI (`jina-ai`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `jina-ai`
- **Alias**: `jina`
- **Display Name**: Jina AI
- **Category**: `apikey`
- **Auth Type**: `apikey`
- **Service Kinds**: `["embedding"]`
- **Website**: `https://jina.ai`
- **API Key URL**: `https://jina.ai/?sui=apikey`
- **Source File**: `open-sse/providers/registry/jina-ai.js`

---

## 2. Endpoints & Configuration
- **Embeddings Endpoint**: `POST https://api.jina.ai/v1/embeddings`
- **Authentication**: `Authorization: Bearer <apiKey>` (`authHeader: "bearer"`)
- **Wire Format**: OpenAI-compatible embeddings payload (`{ model, input, dimensions? }`)

---

## 3. Supported Models
| Model ID | Display Name | Dimensions |
|---|---|---|
| `jina-embeddings-v3` | Jina Embeddings v3 | `1024` |
| `jina-embeddings-v2-base-en` | Jina Embeddings v2 Base EN | `768` |
| `jina-embeddings-v2-base-code` | Jina Embeddings v2 Base Code | `768` |

---

## 4. Verbatim Source Code

### `open-sse/providers/registry/jina-ai.js`
```javascript
export default {
  id: "jina-ai",
  alias: "jina",
  display: {
    name: "Jina AI",
    icon: "blur_on",
    color: "#2563EB",
    textIcon: "JA",
    website: "https://jina.ai",
    notice: {
      text: "10M free tokens on signup (non-commercial), no credit card required.",
      apiKeyUrl: "https://jina.ai/?sui=apikey"
    }
  },
  category: "apikey",
  authType: "apikey",
  serviceKinds: [
    "embedding"
  ],
  embeddingConfig: {
    baseUrl: "https://api.jina.ai/v1/embeddings",
    authType: "apikey",
    authHeader: "bearer",
    models: [
      {
        id: "jina-embeddings-v3",
        name: "Jina Embeddings v3",
        dimensions: 1024
      },
      {
        id: "jina-embeddings-v2-base-en",
        name: "Jina Embeddings v2 Base EN",
        dimensions: 768
      },
      {
        id: "jina-embeddings-v2-base-code",
        name: "Jina Embeddings v2 Base Code",
        dimensions: 768
      }
    ]
  }
};
```
