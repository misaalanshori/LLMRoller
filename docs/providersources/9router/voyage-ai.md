# Voyage AI (`voyage-ai`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `voyage-ai`
- **Display Name**: `Voyage AI`
- **Alias**: `voyage`
- **Category**: `apikey`
- **Service Kinds**: `["embedding"]`
- **Website**: `https://www.voyageai.com`

---

## 2. Endpoints & Authentication

| Parameter | Value |
|---|---|
| **Base URL** | `https://api.voyageai.com/v1/embeddings` |
| **Auth Header** | `Authorization: Bearer <apiKey>` |
| **Format** | OpenAI-compatible `/v1/embeddings` (`{ model, input, encoding_format? }`) |

---

## 3. Registry Definition (`open-sse/providers/registry/voyage-ai.js`)

```javascript
export default {
  id: "voyage-ai",
  priority: 40,
  alias: "voyage-ai",
  uiAlias: "voyage",
  category: "apikey",
  authType: "apikey",
  transport: null,
  models: [
    { id: "voyage-3-large", name: "Voyage 3 Large", kind: "embedding" },
    { id: "voyage-3.5", name: "Voyage 3.5", kind: "embedding" },
    { id: "voyage-3.5-lite", name: "Voyage 3.5 Lite", kind: "embedding" },
    { id: "voyage-code-3", name: "Voyage Code 3", kind: "embedding" },
    { id: "voyage-finance-2", name: "Voyage Finance 2", kind: "embedding" },
    { id: "voyage-law-2", name: "Voyage Law 2", kind: "embedding" },
    { id: "voyage-multilingual-2", name: "Voyage Multilingual 2", kind: "embedding" },
  ],
  serviceKinds: ["embedding"],
  embeddingConfig: { baseUrl: "https://api.voyageai.com/v1/embeddings" },
};
```
