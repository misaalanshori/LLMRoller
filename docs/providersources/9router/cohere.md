# Cohere (`cohere`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `cohere`
- **Priority**: `90`
- **Alias**: `cohere`
- **Display Name**: Cohere
- **Category**: `apikey`
- **Website**: `https://cohere.com`
- **API Key URL**: `https://dashboard.cohere.com/api-keys`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source File**: `open-sse/providers/registry/cohere.js`

---

## 2. Endpoints & Transport
- **Chat Completions Endpoint**: `POST https://api.cohere.ai/v1/chat/completions`
- **Validation / Models Endpoint**: `GET https://api.cohere.ai/v1/models`
- **Wire Format**: `"openai"` (uses Cohere's OpenAI-compatible v1 chat completions endpoint)

---

## 3. Authentication & Headers
- **Authentication**: `Authorization: Bearer <apiKey>`
- **Content-Type**: `application/json`
- **Accept**: `text/event-stream` (streaming) or `application/json` (non-streaming)

---

## 4. Supported Models
| Model ID | Display Name |
|---|---|
| `command-r-plus-08-2024` | Command R+ (Aug 2024) |
| `command-r-08-2024` | Command R (Aug 2024) |
| `command-a-03-2025` | Command A (Mar 2025) |

---

## 5. Verbatim Source Code

### `open-sse/providers/registry/cohere.js`
```javascript
export default {
  id: "cohere",
  priority: 90,
  alias: "cohere",
  display: {
    name: "Cohere",
    icon: "hub",
    color: "#39594D",
    textIcon: "CO",
    website: "https://cohere.com",
    notice: {
      apiKeyUrl: "https://dashboard.cohere.com/api-keys",
    },
  },
  category: "apikey",
  transport: {
    baseUrl: "https://api.cohere.ai/v1/chat/completions",
    validateUrl: "https://api.cohere.ai/v1/models",
  },
  models: [
    { id: "command-r-plus-08-2024", name: "Command R+ (Aug 2024)" },
    { id: "command-r-08-2024", name: "Command R (Aug 2024)" },
    { id: "command-a-03-2025", name: "Command A (Mar 2025)" },
  ],
};
```
