# Morph (`morph`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `morph`
- **Primary Alias**: `morph`
- **Additional Aliases**: `["morphllm"]`
- **UI Alias**: `morph`
- **Display Name**: Morph
- **Category**: `apikey`
- **Auth Type**: `apikey` (`authModes: ["apikey"]`)
- **Website**: `https://morphllm.com`
- **API Key URL**: `https://morphllm.com`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source File**: `open-sse/providers/registry/morph.js`

---

## 2. Endpoints & Transport
- **Chat Completions Endpoint**: `POST https://api.morphllm.com/v1/chat/completions`
- **Validation / Models Endpoint**: `GET https://api.morphllm.com/v1/models`
- **Wire Format**: `"openai"`
- **Authentication**: `Authorization: Bearer <apiKey>`

---

## 3. Supported Models
| Model ID | Display Name | Context Length |
|---|---|---|
| `morph-v3-large` | Morph v3 Large | — |
| `morph-v3-fast` | Morph v3 Fast | — |
| `morph-qwen35-397b` | Qwen 3.5 397B (Morph) | 262,144 |
| `morph-minimax27-230b` | MiniMax M2.7 (Morph) | 200,704 |
| `morph-qwen36-27b` | Qwen 3.6 27B (Morph) | 262,144 |
| `morph-dsv4flash` | DeepSeek V4 Flash (Morph) | 1,048,576 |

---

## 4. Verbatim Source Code

### `open-sse/providers/registry/morph.js`
```javascript
export default {
  id: "morph",
  alias: "morph",
  aliases: ["morphllm"],
  uiAlias: "morph",
  display: {
    name: "Morph",
    icon: "change_history",
    color: "#14B8A6",
    textIcon: "MP",
    website: "https://morphllm.com",
    notice: { apiKeyUrl: "https://morphllm.com" },
  },
  category: "apikey",
  authType: "apikey",
  authModes: ["apikey"],
  transport: {
    baseUrl: "https://api.morphllm.com/v1/chat/completions",
    validateUrl: "https://api.morphllm.com/v1/models",
  },
  models: [
    { id: "morph-v3-large", name: "Morph v3 Large" },
    { id: "morph-v3-fast", name: "Morph v3 Fast" },
    { id: "morph-qwen35-397b", name: "Qwen 3.5 397B (Morph)", contextLength: 262144 },
    { id: "morph-minimax27-230b", name: "MiniMax M2.7 (Morph)", contextLength: 200704 },
    { id: "morph-qwen36-27b", name: "Qwen 3.6 27B (Morph)", contextLength: 262144 },
    { id: "morph-dsv4flash", name: "DeepSeek V4 Flash (Morph)", contextLength: 1048576 },
  ],
};
```
