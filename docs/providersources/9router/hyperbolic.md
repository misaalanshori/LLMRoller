# Hyperbolic (`hyperbolic`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `hyperbolic`
- **Priority**: `160`
- **Primary Alias**: `hyperbolic`
- **Additional Aliases**: `["hyp"]`
- **UI Alias**: `hyp`
- **Display Name**: Hyperbolic
- **Category**: `apikey`
- **Auth Type**: `apikey`
- **Website**: `https://hyperbolic.xyz`
- **API Key URL**: `https://app.hyperbolic.xyz/settings`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source Files**:
  - `open-sse/providers/registry/hyperbolic.js`
  - `open-sse/handlers/ttsProviders/genericFormats.js` (`hyperbolic` TTS format handler)

---

## 2. Endpoints & Transport
- **Chat Completions Endpoint**: `POST https://api.hyperbolic.xyz/v1/chat/completions`
- **Validation / Models Endpoint**: `GET https://api.hyperbolic.xyz/v1/models`
- **Wire Format**: `"openai"`
- **Authentication**: `Authorization: Bearer <apiKey>`

---

## 3. Supported Models
| Model ID | Display Name |
|---|---|
| `Qwen/QwQ-32B` | QwQ 32B |
| `deepseek-ai/DeepSeek-R1` | DeepSeek R1 |
| `deepseek-ai/DeepSeek-V3` | DeepSeek V3 |
| `meta-llama/Llama-3.3-70B-Instruct` | Llama 3.3 70B |
| `meta-llama/Llama-3.2-3B-Instruct` | Llama 3.2 3B |
| `Qwen/Qwen2.5-72B-Instruct` | Qwen 2.5 72B |
| `Qwen/Qwen2.5-Coder-32B-Instruct` | Qwen 2.5 Coder 32B |
| `NousResearch/Hermes-3-Llama-3.1-70B` | Hermes 3 70B |

---

## 4. Verbatim Source Code

### `open-sse/providers/registry/hyperbolic.js`
```javascript
export default {
  id: "hyperbolic",
  priority: 160,
  alias: "hyperbolic",
  aliases: [
    "hyp",
  ],
  uiAlias: "hyp",
  display: {
    name: "Hyperbolic",
    icon: "bolt",
    color: "#00D4FF",
    textIcon: "HY",
    website: "https://hyperbolic.xyz",
    notice: {
      apiKeyUrl: "https://app.hyperbolic.xyz/settings",
    },
  },
  category: "apikey",
  authType: "apikey",
  transport: {
    baseUrl: "https://api.hyperbolic.xyz/v1/chat/completions",
    validateUrl: "https://api.hyperbolic.xyz/v1/models",
  },
  models: [
    { id: "Qwen/QwQ-32B", name: "QwQ 32B" },
    { id: "deepseek-ai/DeepSeek-R1", name: "DeepSeek R1" },
    { id: "deepseek-ai/DeepSeek-V3", name: "DeepSeek V3" },
    { id: "meta-llama/Llama-3.3-70B-Instruct", name: "Llama 3.3 70B" },
    { id: "meta-llama/Llama-3.2-3B-Instruct", name: "Llama 3.2 3B" },
    { id: "Qwen/Qwen2.5-72B-Instruct", name: "Qwen 2.5 72B" },
    { id: "Qwen/Qwen2.5-Coder-32B-Instruct", name: "Qwen 2.5 Coder 32B" },
    { id: "NousResearch/Hermes-3-Llama-3.1-70B", name: "Hermes 3 70B" },
  ],
};
```
