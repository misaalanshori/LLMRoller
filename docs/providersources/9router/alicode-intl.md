# Alibaba Coding Intl (`alicode-intl`) — Upstream Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `alicode-intl`
- **Alias**: `alicode-intl`
- **Display Name**: Alibaba Coding
- **Category**: `apikey`
- **Website**: `https://www.alibabacloud.com/product/coding`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Wire Format**: `openai` (OpenAI Chat Completions compatible)

## 2. Endpoints & Quirks
- **Chat Completions Endpoint**: `POST https://coding-intl.dashscope.aliyuncs.com/v1/chat/completions`
- **Transport Quirks**:
  - `preserveCacheControl: true` — instructs the OpenAI request translator (`open-sse/translator/request/claude-to-openai.js` / `concerns`) to retain `cache_control` blocks on message content parts when translating from Anthropic/Claude format to OpenAI format for DashScope prompt caching.

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

## 5. Source Implementation (`open-sse/providers/registry/alicode-intl.js`)
```javascript
export default {
  id: "alicode-intl",
  priority: 10,
  alias: "alicode-intl",
  display: {
    name: "Alibaba Coding",
    icon: "cloud",
    color: "#FF6A00",
    textIcon: "ALi",
    website: "https://www.alibabacloud.com/product/coding",
    notice: {
      apiKeyUrl: "https://www.alibabacloud.com/product/coding",
    },
  },
  category: "apikey",
  transport: {
    baseUrl: "https://coding-intl.dashscope.aliyuncs.com/v1/chat/completions",
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
