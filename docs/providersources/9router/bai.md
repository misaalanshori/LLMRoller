# B.AI (`bai`) — Upstream Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `bai`
- **Alias**: `bai`
- **Aliases**: `["b-ai"]`
- **UI Alias**: `bai`
- **Display Name**: B.AI
- **Category**: `apikey`
- **Authentication Type**: `apikey`
- **Website**: `https://b.ai`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Wire Format**: `openai` (OpenAI Chat Completions compatible)

## 2. Endpoints & Catalog Discovery
- **Chat Completions Endpoint**: `POST https://api.b.ai/v1/chat/completions`
- **Validation & Live Models Endpoint**: `GET https://api.b.ai/v1/models`
- **Live Models Fetcher**: `{ url: "https://api.b.ai/v1/models", type: "openai" }`
- **Passthrough Models**: `true` (no hardcoded static model IDs; relies on live `/v1/models` fetching and passthrough).

## 3. Authentication & Headers
- **Authentication**: Bearer token (`Authorization: Bearer <apiKey>`)
- **Headers**:
  - `Content-Type: application/json`
  - `Authorization: Bearer <apiKey>`
  - `Accept: text/event-stream` (when streaming)

## 4. Source Implementation (`open-sse/providers/registry/bai.js`)
```javascript
export default {
  id: "bai",
  priority: 120,
  alias: "bai",
  aliases: [
    "b-ai",
  ],
  uiAlias: "bai",
  display: {
    name: "B.AI",
    icon: "account_balance",
    color: "#0369A1",
    textIcon: "BA",
    website: "https://b.ai",
    notice: {
      text: "OpenAI-compatible gateway with one of the larger catalogues here. Accepts a bearer token or an x-api-key header. Model ids are fetched live from the provider.",
      apiKeyUrl: "https://b.ai",
    },
  },
  category: "apikey",
  authType: "apikey",
  transport: {
    baseUrl: "https://api.b.ai/v1/chat/completions",
    validateUrl: "https://api.b.ai/v1/models",
  },
  // No ids hardcoded: the catalogue is large and rotates, so the live endpoint
  // is the source of truth and any id is accepted via passthroughModels.
  modelsFetcher: { url: "https://api.b.ai/v1/models", type: "openai" },
  passthroughModels: true,
};
```
