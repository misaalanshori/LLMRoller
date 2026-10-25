# SambaNova (`sambanova`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `sambanova`
- **Primary Alias**: `samba`
- **Additional Aliases**: `["sambanova-ai"]`
- **UI Alias**: `samba`
- **Hidden**: `true`
- **Display Name**: SambaNova
- **Category**: `apikey`
- **Auth Type**: `apikey` (`authModes: ["apikey"]`)
- **Website**: `https://sambanova.ai`
- **API Key URL**: `https://cloud.sambanova.ai/apis`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source File**: `open-sse/providers/registry/sambanova.js`

---

## 2. Endpoints & Transport
- **Chat Completions Endpoint**: `POST https://api.sambanova.ai/v1/chat/completions`
- **Validation / Models Endpoint**: `GET https://api.sambanova.ai/v1/models`
- **Wire Format**: `"openai"`
- **Authentication**: `Authorization: Bearer <apiKey>`

---

## 3. Supported Models
| Model ID | Display Name | Context Length |
|---|---|---|
| `MiniMax-M2.7` | MiniMax M2.7 | 196,608 |

---

## 4. Verbatim Source Code

### `open-sse/providers/registry/sambanova.js`
```javascript
export default {
  id: "sambanova",
  alias: "samba",
  aliases: ["sambanova-ai"],
  uiAlias: "samba",
  hidden: true,
  display: {
    name: "SambaNova",
    icon: "memory",
    color: "#F97316",
    textIcon: "SN",
    website: "https://sambanova.ai",
    notice: {
      apiKeyUrl: "https://cloud.sambanova.ai/apis",
    },
  },
  category: "apikey",
  authType: "apikey",
  authModes: ["apikey"],
  transport: {
    baseUrl: "https://api.sambanova.ai/v1/chat/completions",
    validateUrl: "https://api.sambanova.ai/v1/models",
  },
  models: [
    { id: "MiniMax-M2.7", name: "MiniMax M2.7", contextLength: 196608 },
  ],
};
```
