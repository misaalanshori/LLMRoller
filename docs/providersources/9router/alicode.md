# Alibaba Coding China (`alicode`) — Upstream Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `alicode`
- **Alias**: `alicode`
- **Display Name**: Alibaba
- **Category**: `apikey`
- **Website**: `https://bailian.console.aliyun.com`
- **API Key URL**: `https://bailian.console.aliyun.com/?apiKey=1`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Wire Format**: `openai` (OpenAI Chat Completions compatible)

## 2. Endpoints & Quirks
- **Chat Completions Endpoint**: `POST https://coding.dashscope.aliyuncs.com/v1/chat/completions`
- **Transport Quirks**:
  - `preserveCacheControl: true` — preserves `cache_control` markers on message content parts when translating from Claude/Anthropic format to OpenAI format.

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
- `qwen3-max-2026-01-23` ("Qwen3 Max")
- `qwen3-coder-next` ("Qwen3 Coder Next")
- `qwen3-coder-plus` ("Qwen3 Coder Plus")
- `glm-4.7` ("GLM 4.7")

## 5. Source Implementation (`open-sse/providers/registry/alicode.js`)
```javascript
export default {
  id: "alicode",
  priority: 20,
  alias: "alicode",
  display: {
    name: "Alibaba",
    icon: "cloud",
    color: "#FF6A00",
    textIcon: "ALi",
    website: "https://bailian.console.aliyun.com",
    notice: {
      apiKeyUrl: "https://bailian.console.aliyun.com/?apiKey=1",
    },
  },
  category: "apikey",
  transport: {
    baseUrl: "https://coding.dashscope.aliyuncs.com/v1/chat/completions",
    headers: {},
    quirks: { preserveCacheControl: true },
  },
  models: [
    { id: "qwen3.5-plus", name: "Qwen3.5 Plus" },
    { id: "kimi-k2.5", name: "Kimi K2.5" },
    { id: "glm-5", name: "GLM 5" },
    { id: "MiniMax-M2.5", name: "MiniMax M2.5" },
    { id: "qwen3-max-2026-01-23", name: "Qwen3 Max" },
    { id: "qwen3-coder-next", name: "Qwen3 Coder Next" },
    { id: "qwen3-coder-plus", name: "Qwen3 Coder Plus" },
    { id: "glm-4.7", name: "GLM 4.7" },
  ],
};
```
