# Vercel AI Gateway (`vercel-ai-gateway`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `vercel-ai-gateway`
- **Display Name**: `Vercel AI Gateway`
- **Alias**: `vercel`
- **Category**: `apikey`
- **Service Kinds**: `["llm", "embedding", "image", "imageToText", "webSearch"]`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Website**: `https://vercel.com/ai-gateway`

---

## 2. Endpoints & Authentication

| Service | Endpoint |
|---|---|
| **Chat Completions** | `POST https://ai-gateway.vercel.sh/v1/chat/completions` |
| **Embeddings** | `POST https://ai-gateway.vercel.sh/v1/embeddings` |
| **Image Generation** | `POST https://ai-gateway.vercel.sh/v1/images/generations` |
| **Models Discovery** | `GET https://ai-gateway.vercel.sh/v1/models` (`modelsFetcher`) |
| **Usage / Credits** | `GET https://ai-gateway.vercel.sh/v1/credits` |
| **Authentication** | `Authorization: Bearer <apiKey>` |
| **Thinking Format** | `"openai"` |
| **Rate Limit Retry** | `retry: { "429": 2 }` |

---

## 3. Registry Definition (`open-sse/providers/registry/vercel-ai-gateway.js`)

```javascript
export default {
  id: "vercel-ai-gateway",
  priority: 160,
  alias: "vercel-ai-gateway",
  aliases: ["vercel"],
  uiAlias: "vercel",
  category: "apikey",
  transport: {
    baseUrl: "https://ai-gateway.vercel.sh/v1/chat/completions",
    thinkingFormat: "openai",
    retry: { "429": 2 },
    usage: { url: "https://ai-gateway.vercel.sh/v1/credits" },
  },
  serviceKinds: ["llm", "embedding", "image", "imageToText", "webSearch"],
  embeddingConfig: { baseUrl: "https://ai-gateway.vercel.sh/v1/embeddings" },
  imageConfig: { baseUrl: "https://ai-gateway.vercel.sh/v1/images/generations" },
  searchViaChat: { defaultModel: "openai/gpt-4o-mini", pricingUrl: "https://vercel.com/docs/ai-gateway/pricing" },
  modelsFetcher: { url: "https://ai-gateway.vercel.sh/v1/models", type: "openai" },
  passthroughModels: true,
  features: {
    usage: true,
    usageApikey: true,
  },
};
```
