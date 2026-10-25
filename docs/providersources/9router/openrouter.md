# OpenRouter (`openrouter`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `openrouter`
- **Priority**: `10` (`hasFree: true`)
- **Alias**: `openrouter`
- **Display Name**: OpenRouter
- **Category**: `freeTier`
- **Auth Type**: `apikey` (`authModes: ["apikey"]`)
- **Service Kinds**: `["llm", "embedding", "tts", "imageToText", "video", "systemone"]` (plus `imageConfig`)
- **Website**: `https://openrouter.ai`
- **API Key URL**: `https://openrouter.ai/settings/keys`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source File**: `open-sse/providers/registry/openrouter.js`

---

## 2. Endpoints by Service Kind
1. **LLM & Vision Chat Completions**: `POST https://openrouter.ai/api/v1/chat/completions` (`thinkingFormat: "openai"`)
2. **Embeddings (`embeddingConfig`)**: `POST https://openrouter.ai/api/v1/embeddings`
3. **Text-to-Speech (`ttsConfig`)**: `POST https://openrouter.ai/api/v1/chat/completions` (`defaultModel: "openai/gpt-4o-mini-tts"`)
4. **Image Generation (`imageConfig`)**: `POST https://openrouter.ai/api/v1/images/generations`
5. **Async Video Generation (`videoConfig`)**: `POST https://openrouter.ai/api/v1/videos` -> polls `GET https://openrouter.ai/api/v1/videos/{id}`
6. **System One Decision API (`systemoneConfig`)**: `POST https://openrouter.ai/api/v1/systemone`
7. **Dynamic Models Fetcher**: `GET https://openrouter.ai/api/v1/models` (`type: "openrouter-free"`, `passthroughModels: true`)

---

## 3. Authentication & Attribution Headers
- **Authorization Header**: `Authorization: Bearer <apiKey>`
- **Static Attribution Headers (on all endpoints)**:
  - `HTTP-Referer`: `"https://endpoint-proxy.local"`
  - `X-Title`: `"Endpoint Proxy"`

---

## 4. Seed Models (Plus Dynamic Catalog via `modelsFetcher`)
| Model ID | Display Name | Kind |
|---|---|---|
| `openai/text-embedding-3-large`, `openai/text-embedding-3-small`, `openai/text-embedding-ada-002`, `qwen/qwen3-embedding-8b`, `perplexity/pplx-embed-v1-4b`, `perplexity/pplx-embed-v1-0.6b`, `nvidia/llama-nemotron-embed-vl-1b-v2:free` | Embeddings | `embedding` |
| `openai/gpt-4o-mini-tts`, `openai/tts-1-hd`, `openai/tts-1` | TTS | `tts` |
| `openai/dall-e-3`, `openai/gpt-image-1`, `google/imagen-3.0-generate-002`, `black-forest-labs/FLUX.1-schnell` | Images | `image` |
| `google/veo-3.1`, `openai/sora-2-pro`, `bytedance/seedance-2.0` | Video | `video` |
| `typesafe/jev-1.13` | Jev 1.13 | `systemone` |

---

## 5. Verbatim Source Code

### `open-sse/providers/registry/openrouter.js`
```javascript
export default {
  id: "openrouter",
  priority: 10,
  hasFree: true,
  alias: "openrouter",
  display: {
    name: "OpenRouter",
    icon: "router",
    color: "#F97316",
    textIcon: "OR",
    website: "https://openrouter.ai",
    notice: {
      text: "Free tier: 27+ free models, no credit card needed, 200 req/day. After $10 credit: 1,000 req/day.",
      apiKeyUrl: "https://openrouter.ai/settings/keys",
    },
  },
  category: "freeTier",
  authType: "apikey",
  authModes: ["apikey"],
  transport: {
    baseUrl: "https://openrouter.ai/api/v1/chat/completions",
    thinkingFormat: "openai",
    headers: {
      "HTTP-Referer": "https://endpoint-proxy.local",
      "X-Title": "Endpoint Proxy",
    },
  },
  serviceKinds: ["llm","embedding","tts","imageToText","video","systemone"],
  systemoneConfig: {
    baseUrl: "https://openrouter.ai/api/v1/systemone",
    headers: {"HTTP-Referer":"https://endpoint-proxy.local","X-Title":"Endpoint Proxy"},
  },
  ttsConfig: {
    baseUrl: "https://openrouter.ai/api/v1/chat/completions",
    defaultModel: "openai/gpt-4o-mini-tts",
    headers: {"HTTP-Referer":"https://endpoint-proxy.local","X-Title":"Endpoint Proxy"},
  },
  embeddingConfig: {
    baseUrl: "https://openrouter.ai/api/v1/embeddings",
    authType: "apikey",
    authHeader: "bearer",
    headers: {"HTTP-Referer":"https://endpoint-proxy.local","X-Title":"Endpoint Proxy"},
  },
  imageConfig: {
    baseUrl: "https://openrouter.ai/api/v1/images/generations",
    headers: {"HTTP-Referer":"https://endpoint-proxy.local","X-Title":"Endpoint Proxy"},
  },
  videoConfig: {
    baseUrl: "https://openrouter.ai/api/v1/videos",
    headers: {"HTTP-Referer":"https://endpoint-proxy.local","X-Title":"Endpoint Proxy"},
  },
  modelsFetcher: { url: "https://openrouter.ai/api/v1/models", type: "openrouter-free" },
  passthroughModels: true,
};
```
