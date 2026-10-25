# API.airforce (`api-airforce`) — Upstream Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `api-airforce`
- **Alias**: `af`
- **Aliases**: `["airforce"]`
- **UI Alias**: `af`
- **Display Name**: API.airforce
- **Category**: `freeTier`
- **Authentication Type**: `apikey`
- **Website**: `https://api.airforce`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Wire Format**: `openai` (OpenAI Chat Completions compatible)

## 2. Endpoints & Routing
- **Chat Completions Endpoint**: `POST https://api.airforce/v1/chat/completions`
- **Validation & Models Catalog Endpoint**: `GET https://api.airforce/v1/models`
- **Dynamic Models Fetcher**: `{ url: "https://api.airforce/v1/models", type: "airforce-free" }`
- **Passthrough Models**: `true`
- **Force Stream**: `true` (`transport.forceStream: true` — forces upstream requests to use `stream: true` even when client requests non-streaming, converting the SSE stream back to JSON if needed).

## 3. Authentication & Headers
- **Authentication**: Bearer token (`Authorization: Bearer <apiKey>`)
- **Custom Static Headers**:
  - `Content-Type: application/json`
  - `Authorization: Bearer <apiKey>`
  - `HTTP-Referer: https://endpoint-proxy.local`
  - `X-Title: Endpoint Proxy`
  - `Accept: text/event-stream`

## 4. Seeded Models
- `gpt-oss-120b` ("GPT-OSS 120B (Free)", `contextLength: 131072`)
- `gpt-oss-20b` ("GPT-OSS 20B (Free)", `contextLength: 131072`)
- `kimi-k2.7-code` ("Kimi K2.7 Code (Free)", `contextLength: 262144`)

## 5. Source Implementation (`open-sse/providers/registry/api-airforce.js`)
```javascript
export default {
  id: "api-airforce",
  alias: "af",
  aliases: [
    "airforce",
  ],
  uiAlias: "af",
  display: {
    name: "API.airforce",
    icon: "flight",
    color: "#0EA5E9",
    textIcon: "AF",
    website: "https://api.airforce",
    notice: {
      apiKeyUrl: "https://api.airforce",
    },
  },
  category: "freeTier",
  authType: "apikey",
  authModes: [
    "apikey",
  ],
  passthroughModels: true,
  modelsFetcher: { url: "https://api.airforce/v1/models", type: "airforce-free" },
  transport: {
    baseUrl: "https://api.airforce/v1/chat/completions",
    validateUrl: "https://api.airforce/v1/models",
    headers: {
      "HTTP-Referer": "https://endpoint-proxy.local",
      "X-Title": "Endpoint Proxy",
    },
    forceStream: true,
  },
  models: [
    { id: "gpt-oss-120b", name: "GPT-OSS 120B (Free)", contextLength: 131072 },
    { id: "gpt-oss-20b", name: "GPT-OSS 20B (Free)", contextLength: 131072 },
    { id: "kimi-k2.7-code", name: "Kimi K2.7 Code (Free)", contextLength: 262144 },
  ],
};
```
