# Poolside (`poolside`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `poolside`
- **Priority**: `60`
- **Primary Alias**: `poolside`
- **Additional Aliases**: `["ps"]`
- **UI Alias**: `ps`
- **Display Name**: Poolside
- **Category**: `freeTier`
- **Auth Type**: `apikey` (`authModes: ["apikey"]`)
- **Website**: `https://poolside.ai`
- **API Key URL**: `https://platform.poolside.ai/api-keys`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source File**: `open-sse/providers/registry/poolside.js`

---

## 2. Endpoints & Transport
- **Chat Completions Endpoint**: `POST https://inference.poolside.ai/v1/chat/completions`
- **Validation / Models Endpoint**: `GET https://inference.poolside.ai/v1/models`
- **Wire Format**: `"openai"`
- **Authentication**: `Authorization: Bearer <apiKey>`

---

## 3. Supported Models
| Model ID | Display Name |
|---|---|
| `poolside/laguna-s-2.1` | Laguna S 2.1 |
| `poolside/laguna-xs-2.1` | Laguna XS 2.1 |

---

## 4. Verbatim Source Code

### `open-sse/providers/registry/poolside.js`
```javascript
export default {
  id: "poolside",
  priority: 60,
  alias: "poolside",
  aliases: [
    "ps",
  ],
  uiAlias: "ps",
  display: {
    name: "Poolside",
    icon: "water_drop",
    color: "#0EA5E9",
    textIcon: "PS",
    website: "https://poolside.ai",
    notice: {
      apiKeyUrl: "https://platform.poolside.ai/api-keys",
    },
  },
  category: "freeTier",
  authType: "apikey",
  authModes: ["apikey"],
  transport: {
    baseUrl: "https://inference.poolside.ai/v1/chat/completions",
    validateUrl: "https://inference.poolside.ai/v1/models",
  },
  models: [
    { id: "poolside/laguna-s-2.1", name: "Laguna S 2.1" },
    { id: "poolside/laguna-xs-2.1", name: "Laguna XS 2.1" },
  ],
};
```
