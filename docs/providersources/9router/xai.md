# xAI (Grok) (`xai`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `xai`
- **Display Name**: `xAI (Grok)`
- **Alias**: `xai`
- **Category**: `oauth` (`authModes: ["oauth", "apikey"]`)
- **Service Kinds**: `["llm", "imageToText", "webSearch", "image", "video"]`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Website**: `https://x.ai`

---

## 2. Endpoints & Authentication

| Service | Endpoint |
|---|---|
| **Chat Completions** | `POST https://api.x.ai/v1/chat/completions` |
| **Responses API / Web Search** | `POST https://api.x.ai/v1/responses` |
| **Image Generation** | `POST https://api.x.ai/v1/images/generations` |
| **Video Generation (Async)** | `POST https://api.x.ai/v1/videos` |
| **Models / Validation URL** | `GET https://api.x.ai/v1/models` |
| **OAuth2 Token / Refresh URL** | `POST https://auth.x.ai/oauth2/token` (`clientId: "b1a00492-073a-47ea-816f-4c329264a828"`) |
| **Authentication** | `Authorization: Bearer <accessToken \| apiKey>` |

---

## 3. Registry Definition (`open-sse/providers/registry/xai.js`)

```javascript
export default {
  id: "xai",
  priority: 280,
  alias: "xai",
  category: "oauth",
  authModes: ["oauth", "apikey"],
  hasOAuth: true,
  transport: {
    baseUrl: "https://api.x.ai/v1/chat/completions",
    validateUrl: "https://api.x.ai/v1/models",
    responsesUrl: "https://api.x.ai/v1/responses",
    clientId: "b1a00492-073a-47ea-816f-4c329264a828",
    tokenUrl: "https://auth.x.ai/oauth2/token",
    refreshUrl: "https://auth.x.ai/oauth2/token",
  },
  models: [
    { id: "grok-4.7", name: "Grok 4.7" },
    { id: "grok-4.6", name: "Grok 4.6" },
    { id: "grok-4.5", name: "Grok 4.5" },
    { id: "grok-4", name: "Grok 4" },
    { id: "grok-4-fast-reasoning", name: "Grok 4 Fast Reasoning" },
    { id: "grok-code-fast-1", name: "Grok Code Fast" },
    { id: "grok-3", name: "Grok 3" },
    { id: "grok-2-image-1212", name: "Grok 2 Image", params: ["n", "response_format"], kind: "image" },
    { id: "grok-imagine-video", name: "Grok Imagine Video", params: ["duration", "aspect_ratio", "resolution"], kind: "video" },
  ],
  serviceKinds: ["llm", "imageToText", "webSearch", "image", "video"],
  imageConfig: { baseUrl: "https://api.x.ai/v1/images/generations", bodyFields: ["model", "prompt", "n", "response_format"] },
  videoConfig: { baseUrl: "https://api.x.ai/v1/videos" },
  searchViaChat: {
    defaultModel: "grok-4.20-reasoning",
    endpoint: "https://api.x.ai/v1/responses",
    pricingUrl: "https://x.ai/api#pricing",
  },
};
```
