# Cerebras (`cerebras`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `cerebras`
- **Priority**: `60`
- **Alias**: `cerebras`
- **Display Name**: Cerebras
- **Category**: `apikey`
- **Website**: `https://www.cerebras.ai`
- **API Key URL**: `https://cloud.cerebras.ai/platform`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source File**: `open-sse/providers/registry/cerebras.js`

---

## 2. Endpoints & Transport
- **Chat Completions Endpoint**: `POST https://api.cerebras.ai/v1/chat/completions`
- **Validation / Models Endpoint**: `GET https://api.cerebras.ai/v1/models`
- **Wire Format**: `"openai"`

---

## 3. Authentication, Headers & Quirks
- **Authentication**: `Authorization: Bearer <apiKey>`
- **Protocol Quirks**:
  - `dropClientMetadata: true`: Instructs the request translator/parameter sanitizer to strip client-side `metadata` and `user` fields before sending the payload to Cerebras, as Cerebras rejects unrecognized top-level metadata parameters.

---

## 4. Supported Models
| Model ID | Display Name |
|---|---|
| `gpt-oss-120b` | GPT OSS 120B |
| `zai-glm-4.7` | ZAI GLM 4.7 |
| `llama-3.3-70b` | Llama 3.3 70B |
| `llama-4-scout-17b-16e-instruct` | Llama 4 Scout |
| `qwen-3-235b-a22b-instruct-2507` | Qwen3 235B A22B |
| `qwen-3-32b` | Qwen3 32B |

---

## 5. Verbatim Source Code

### `open-sse/providers/registry/cerebras.js`
```javascript
export default {
  id: "cerebras",
  priority: 60,
  alias: "cerebras",
  display: {
    name: "Cerebras",
    icon: "memory",
    color: "#FF4F00",
    textIcon: "CB",
    website: "https://www.cerebras.ai",
    notice: {
      apiKeyUrl: "https://cloud.cerebras.ai/platform",
    },
  },
  category: "apikey",
  transport: {
    baseUrl: "https://api.cerebras.ai/v1/chat/completions",
    validateUrl: "https://api.cerebras.ai/v1/models",
    quirks: {
      dropClientMetadata: true,
    },
  },
  models: [
    { id: "gpt-oss-120b", name: "GPT OSS 120B" },
    { id: "zai-glm-4.7", name: "ZAI GLM 4.7" },
    { id: "llama-3.3-70b", name: "Llama 3.3 70B" },
    { id: "llama-4-scout-17b-16e-instruct", name: "Llama 4 Scout" },
    { id: "qwen-3-235b-a22b-instruct-2507", name: "Qwen3 235B A22B" },
    { id: "qwen-3-32b", name: "Qwen3 32B" },
  ],
};
```
