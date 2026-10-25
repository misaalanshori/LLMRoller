# Atria Dawn (`atria`) — Upstream Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `atria`
- **Alias**: `atria`
- **Aliases**: `["atria-asi"]`
- **UI Alias**: `atria`
- **Display Name**: Atria Dawn
- **Category**: `apikey`
- **Authentication Type**: `apikey`
- **Website**: `https://atria-asi.ai`
- **API Key URL**: `https://api.atria-asi.ai/dashboard`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Wire Format**: `openai` (OpenAI Chat Completions compatible)

## 2. Endpoints & Routing
- **Chat Completions Endpoint**: `POST https://api.atria-asi.ai/v1/chat/completions`
- **Validation / Models Endpoint**: `GET https://api.atria-asi.ai/v1/models`
- **Passthrough Models**: `true`

## 3. Authentication & Headers
- **Authentication**: Bearer token (`Authorization: Bearer <apiKey>`)
- **Headers**:
  - `Content-Type: application/json`
  - `Authorization: Bearer <apiKey>`
  - `Accept: text/event-stream` (when streaming)

## 4. Supported Models & Constraints
- `Atria-Dawn-Preview` ("Atria Dawn Preview" — case-sensitive model ID; text-only, upstream blocks image/PDF inputs).

## 5. Source Implementation (`open-sse/providers/registry/atria.js`)
```javascript
export default {
  id: "atria",
  priority: 120,
  alias: "atria",
  aliases: [
    "atria-asi",
  ],
  uiAlias: "atria",
  display: {
    name: "Atria Dawn",
    icon: "flare",
    color: "#C2410C",
    textIcon: "AD",
    website: "https://atria-asi.ai",
    notice: {
      text: "OpenAI-compatible endpoint from Atria Dawn (AtomInnoLab). Currently a research preview offering a single text model, Atria-Dawn-Preview.",
      apiKeyUrl: "https://api.atria-asi.ai/dashboard",
    },
  },
  category: "apikey",
  authType: "apikey",
  transport: {
    baseUrl: "https://api.atria-asi.ai/v1/chat/completions",
    validateUrl: "https://api.atria-asi.ai/v1/models",
  },
  // Docs pin the model field to one case-sensitive id. Text-only for now: the
  // service ships a hook that blocks image/PDF input, so no vision is claimed.
  models: [
    { id: "Atria-Dawn-Preview", name: "Atria Dawn Preview" },
  ],
  passthroughModels: true,
};
```
