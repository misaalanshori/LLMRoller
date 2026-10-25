# Agnes AI (`agnes`) — Upstream Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `agnes`
- **Aliases**: `agnes`, `agnes-ai`
- **UI Alias**: `agnes`
- **Display Name**: Agnes AI
- **Category**: `freeTier`
- **Authentication Type**: `apikey`
- **Website**: `https://agnes-ai.com`
- **API Key Issuance URL**: `https://platform.agnes-ai.com`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Wire Format**: `openai` (OpenAI Chat Completions compatible)

## 2. Endpoints & Routing
- **Chat Completions Endpoint**: `POST https://apihub.agnes-ai.com/v1/chat/completions`
- **Model Validation / Catalog Endpoint**: `GET https://apihub.agnes-ai.com/v1/models`
- **Passthrough Models**: `true` (client may request any model ID beyond the seeded static catalog).

## 3. Authentication & Headers
- **Default Auth Mechanism**: Standard Bearer token via `DefaultExecutor` (`Authorization: Bearer <apiKey || accessToken>`). Note: Upstream notice mentions it accepts either a Bearer token or an `x-api-key` header; `9router`'s `DefaultExecutor` sends `Authorization: Bearer <token>`.
- **Request Headers**:
  - `Content-Type: application/json`
  - `Authorization: Bearer <token>`
  - `Accept: text/event-stream` (when `stream: true`)

## 4. Seeded Models
The live catalog at `https://apihub.agnes-ai.com/v1/models` requires authentication (returns HTTP 401 `"Token not provided"` without a token), so `9router` seeds the following default models while enabling `passthroughModels: true`:
- `agnes-2.5-flash` ("Agnes 2.5 Flash")
- `agnes-2.5-pro` ("Agnes 2.5 Pro")
- `agnes-2.5-pro-beta` ("Agnes 2.5 Pro Beta")
- `agnes-3.0-flash` ("Agnes 3.0 Flash")

## 5. Request & Response Format
Uses standard OpenAI `/v1/chat/completions` JSON request and SSE response formatting via `DefaultExecutor`.

## 6. Source Implementation (`open-sse/providers/registry/agnes.js`)
```javascript
export default {
  id: "agnes",
  priority: 120,
  alias: "agnes",
  aliases: [
    "agnes-ai",
  ],
  uiAlias: "agnes",
  display: {
    name: "Agnes AI",
    icon: "auto_awesome",
    color: "#7C3AED",
    textIcon: "AG",
    website: "https://agnes-ai.com",
    notice: {
      text: "OpenAI-compatible gateway from Agnes AI, offering free API credits on sign-up. Accepts a bearer token or an x-api-key header.",
      apiKeyUrl: "https://platform.agnes-ai.com",
    },
  },
  category: "freeTier",
  authType: "apikey",
  transport: {
    baseUrl: "https://apihub.agnes-ai.com/v1/chat/completions",
    validateUrl: "https://apihub.agnes-ai.com/v1/models",
  },
  // Seeds so the dashboard has something to show before a key is saved and
  // /v1/models answers with a usable list. The live catalogue at
  // apihub.agnes-ai.com/v1/models requires a token (401 "Token not provided"),
  // so these ids are a curated starting set rather than a verified dump —
  // passthroughModels below still accepts any id the account actually has.
  models: [
    { id: "agnes-2.5-flash", name: "Agnes 2.5 Flash" },
    { id: "agnes-2.5-pro", name: "Agnes 2.5 Pro" },
    { id: "agnes-2.5-pro-beta", name: "Agnes 2.5 Pro Beta" },
    { id: "agnes-3.0-flash", name: "Agnes 3.0 Flash" },
  ],
  passthroughModels: true,
};
```
