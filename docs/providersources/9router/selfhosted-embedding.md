# Self-hosted Embedding (`selfhosted-embedding`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `selfhosted-embedding`
- **Display Name**: `Self-hosted Embedding`
- **Alias**: `she`
- **Category**: `apikey`
- **Service Kinds**: `["embedding"]`

---

## 2. Endpoints & Authentication

| Parameter | Value |
|---|---|
| **Default Base URL** | `http://localhost:8080/v1/embeddings` (configurable via `providerSpecificData.baseUrl`) |
| **Auth Method** | Optional `Authorization: Bearer <apiKey>` (`authType: "apikey"`, `authHeader: "bearer"`) |
| **Supported Model** | `embedding` (`Self-hosted Embedding`, `dimensions: null`) |

---

## 3. Implementation (`open-sse/config/embeddingRegistry.js` & `src/app/v1/embeddings/route.js`)

```javascript
"selfhosted-embedding": {
  id: "selfhosted-embedding",
  name: "Self-hosted Embedding",
  baseUrl: "http://localhost:8080/v1/embeddings",
  authType: "apikey",
  authHeader: "bearer",
  models: [
    { id: "embedding", name: "Self-hosted Embedding", dimensions: null },
  ],
},
```

### Dynamic Base URL Resolution (`src/app/v1/embeddings/route.js`)

```javascript
let targetUrl = providerConfig.baseUrl;
if (providerId === "selfhosted-embedding" && credentials.providerSpecificData?.baseUrl) {
  const customBase = credentials.providerSpecificData.baseUrl.replace(/\/$/, "");
  targetUrl = customBase.endsWith("/embeddings") ? customBase : `${customBase}/embeddings`;
}
```
