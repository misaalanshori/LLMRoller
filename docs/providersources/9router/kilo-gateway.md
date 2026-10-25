# Kilo Gateway (`kilo-gateway`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `kilo-gateway`
- **Primary Alias**: `kgw`
- **Additional Aliases**: `["kilo-gateway", "kilogateway"]`
- **UI Alias**: `kgw`
- **Display Name**: Kilo Gateway
- **Category**: `freeTier`
- **Auth Type**: `apikey` (`authModes: ["apikey"]`)
- **Website**: `https://kilo.ai`
- **API Key URL**: `https://kilo.ai/dashboard?tab=apiKeys`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source File**: `open-sse/providers/registry/kilo-gateway.js`

---

## 2. Endpoints & Transport
- **Chat Completions Endpoint**: `POST https://api.kilo.ai/api/gateway/chat/completions`
- **Validation / Models Endpoint**: `GET https://api.kilo.ai/api/gateway/models`
- **Wire Format**: `"openai"`
- **Authentication**: `Authorization: Bearer <apiKey>`

---

## 3. Supported Models
| Model ID | Display Name | Context Length |
|---|---|---|
| `kilo-auto/free` | Kilo Auto Free | 256,000 |
| `nvidia/nemotron-3-super-120b-a12b:free` | Nemotron 3 Super 120B (Free) | 262,144 |
| `nvidia/nemotron-3-ultra-550b-a55b:free` | Nemotron 3 Ultra 550B (Free) | 1,000,000 |
| `kwaipilot/kat-coder-pro-v2.5:free` | Kat Coder Pro v2.5 (Free) | 256,000 |
| `kilo-auto/frontier` | Kilo Auto Frontier | 1,000,000 |
| `kilo-auto/balanced` | Kilo Auto Balanced | 1,000,000 |

---

## 4. Verbatim Source Code

### `open-sse/providers/registry/kilo-gateway.js`
```javascript
export default {
  id: "kilo-gateway",
  alias: "kgw",
  aliases: [
    "kilo-gateway",
    "kilogateway",
  ],
  uiAlias: "kgw",
  category: "freeTier",
  display: {
    name: "Kilo Gateway",
    icon: "login",
    color: "#8B5CF6",
    textIcon: "KG",
    website: "https://kilo.ai",
    notice: {
      apiKeyUrl: "https://kilo.ai/dashboard?tab=apiKeys",
    },
  },
  authType: "apikey",
  authModes: ["apikey"],
  transport: {
    baseUrl: "https://api.kilo.ai/api/gateway/chat/completions",
    validateUrl: "https://api.kilo.ai/api/gateway/models",
  },
  models: [
    { id: "kilo-auto/free", name: "Kilo Auto Free", contextLength: 256000 },
    { id: "nvidia/nemotron-3-super-120b-a12b:free", name: "Nemotron 3 Super 120B (Free)", contextLength: 262144 },
    { id: "nvidia/nemotron-3-ultra-550b-a55b:free", name: "Nemotron 3 Ultra 550B (Free)", contextLength: 1000000 },
    { id: "kwaipilot/kat-coder-pro-v2.5:free", name: "Kat Coder Pro v2.5 (Free)", contextLength: 256000 },
    { id: "kilo-auto/frontier", name: "Kilo Auto Frontier", contextLength: 1000000 },
    { id: "kilo-auto/balanced", name: "Kilo Auto Balanced", contextLength: 1000000 },
  ],
};
```
