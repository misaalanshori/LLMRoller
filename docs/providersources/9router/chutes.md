# Chutes AI (`chutes`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `chutes`
- **Priority**: `70`
- **Primary Alias**: `chutes`
- **Additional Aliases**: `["ch"]`
- **UI Alias**: `ch`
- **Display Name**: Chutes AI
- **Category**: `apikey`
- **Website**: `https://chutes.ai`
- **API Key URL**: `https://chutes.ai/app/api`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source File**: `open-sse/providers/registry/chutes.js`

---

## 2. Endpoints & Transport
- **Chat Completions Endpoint**: `POST https://llm.chutes.ai/v1/chat/completions`
- **Validation / Models Endpoint**: `GET https://llm.chutes.ai/v1/models`
- **Wire Format**: `"openai"`

---

## 3. Authentication & Headers
- **Authentication**: `Authorization: Bearer <apiKey>`
- **Content-Type**: `application/json`
- **Accept**: `text/event-stream` (streaming) or `application/json` (non-streaming)

---

## 4. Models
- Chutes AI uses dynamic model discovery via `https://llm.chutes.ai/v1/models` or user-supplied model IDs prefixed with `chutes/` or `ch/`.

---

## 5. Verbatim Source Code

### `open-sse/providers/registry/chutes.js`
```javascript
export default {
  id: "chutes",
  priority: 70,
  alias: "chutes",
  aliases: [
    "ch",
  ],
  uiAlias: "ch",
  display: {
    name: "Chutes AI",
    icon: "water_drop",
    color: "#ffffffff",
    textIcon: "CH",
    website: "https://chutes.ai",
    notice: {
      apiKeyUrl: "https://chutes.ai/app/api",
    },
  },
  category: "apikey",
  transport: {
    baseUrl: "https://llm.chutes.ai/v1/chat/completions",
    validateUrl: "https://llm.chutes.ai/v1/models",
  },
};
```
