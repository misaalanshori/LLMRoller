# Alibaba Model Studio Intl (`alims-intl`) — Upstream Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `alims-intl`
- **Alias**: `alims-intl`
- **Display Name**: Alibaba Studio
- **Category**: `apikey`
- **Website**: `https://modelstudio.console.alibabacloud.com`
- **API Key URL**: `https://modelstudio.console.alibabacloud.com/?apiKey=1`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Wire Format**: `openai` (OpenAI Chat Completions compatible)

## 2. Endpoints & Quirks
- **Chat Completions Endpoint**: `POST https://dashscope-intl.aliyuncs.com/compatible-mode/v1/chat/completions`
- **Host Separation Note**: Uses standard DashScope API keys (`sk-...`), distinct from Coding Plan keys (`alicode-intl` on `coding-intl.dashscope.aliyuncs.com`) and Token Plan keys (`alitp-intl`).
- **Transport Quirks**:
  - `preserveCacheControl: true` — preserves `cache_control` markers on message content parts when translating to OpenAI format.

## 3. Authentication & Headers
- **Authentication**: Bearer token (`Authorization: Bearer <apiKey>`)
- **Headers**:
  - `Content-Type: application/json`
  - `Authorization: Bearer <apiKey>`
  - `Accept: text/event-stream` (when streaming)

## 4. Supported Models
- `qwen3.5-plus` ("Qwen3.5 Plus")
- `kimi-k2.5` ("Kimi K2.5")
- `glm-5` ("GLM 5")
- `MiniMax-M2.5` ("MiniMax M2.5")
- `qwen3-coder-next` ("Qwen3 Coder Next")
- `qwen3-coder-plus` ("Qwen3 Coder Plus")
- `glm-4.7` ("GLM 4.7")

## 5. Source Implementation (`open-sse/providers/registry/alims-intl.js`)
```javascript
// Model Studio Intl — standard DashScope API keys (sk-...), NOT Coding Plan keys.
// Sibling of alicode-intl (Coding Plan). Two key types use two different hosts.
export default {
  id: "alims-intl",
  priority: 11,
  alias: "alims-intl",
  display: {
    name: "Alibaba Studio",
    icon: "cloud",
    color: "#FF6A00",
    textIcon: "ALi",
    website: "https://modelstudio.console.alibabacloud.com",
    notice: {
      apiKeyUrl: "https://modelstudio.console.alibabacloud.com/?apiKey=1",
    },
  },
  category: "apikey",
  transport: {
    baseUrl: "https://dashscope-intl.aliyuncs.com/compatible-mode/v1/chat/completions",
    headers: {},
    quirks: { preserveCacheControl: true },
  },
  models: [
    { id: "qwen3.5-plus", name: "Qwen3.5 Plus" },
    { id: "kimi-k2.5", name: "Kimi K2.5" },
    { id: "glm-5", name: "GLM 5" },
    { id: "MiniMax-M2.5", name: "MiniMax M2.5" },
    { id: "qwen3-coder-next", name: "Qwen3 Coder Next" },
    { id: "qwen3-coder-plus", name: "Qwen3 Coder Plus" },
    { id: "glm-4.7", name: "GLM 4.7" },
  ],
};
```
