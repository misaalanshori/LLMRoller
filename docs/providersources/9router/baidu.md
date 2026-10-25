# Baidu Qianfan (`baidu`) — Upstream Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `baidu`
- **Alias**: `qianfan`
- **Aliases**: `["qianfan", "ernie", "baidu-qianfan"]`
- **UI Alias**: `qianfan`
- **Display Name**: Baidu Qianfan
- **Category**: `apikey`
- **Authentication Type**: `apikey`
- **Website**: `https://cloud.baidu.com/product/qianfan.html`
- **API Key URL**: `https://console.bce.baidu.com/qianfan/ais/console/applicationConsole/application`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Wire Format**: `openai` (OpenAI v2 Chat Completions compatible)

## 2. Endpoints
- **Chat Completions Endpoint**: `POST https://qianfan.baidubce.com/v2/chat/completions`
- **Validation / Models Endpoint**: `GET https://qianfan.baidubce.com/v2/models`

## 3. Authentication & Headers
- **Authentication**: Bearer token (`Authorization: Bearer <apiKey>`)
- **Headers**:
  - `Content-Type: application/json`
  - `Authorization: Bearer <apiKey>`
  - `Accept: text/event-stream` (when streaming)

## 4. Supported Models
- `deepseek-v4-pro` ("DeepSeek V4 Pro", `contextLength: 1048576`)
- `deepseek-v4-flash` ("DeepSeek V4 Flash", `contextLength: 1048576`)
- `glm-5.2` ("GLM 5.2", `contextLength: 512000`)
- `glm-5.1` ("GLM 5.1", `contextLength: 198000`)
- `kimi-k2.6` ("Kimi K2.6", `contextLength: 262144`)
- `qwen3.5-397b-a17b` ("Qwen 3.5 397B A17B", `contextLength: 262144`)
- `qwen3.5-27b` ("Qwen 3.5 27B", `contextLength: 262144`)

## 5. Source Implementation (`open-sse/providers/registry/baidu.js`)
```javascript
export default {
  id: "baidu",
  alias: "qianfan",
  aliases: ["qianfan", "ernie", "baidu-qianfan"],
  uiAlias: "qianfan",
  category: "apikey",
  authType: "apikey",
  authModes: ["apikey"],
  display: {
    name: "Baidu Qianfan",
    icon: "search",
    color: "#2932E1",
    textIcon: "BD",
    website: "https://cloud.baidu.com/product/qianfan.html",
    notice: {
      apiKeyUrl:
        "https://console.bce.baidu.com/qianfan/ais/console/applicationConsole/application",
    },
  },
  transport: {
    baseUrl: "https://qianfan.baidubce.com/v2/chat/completions",
    validateUrl: "https://qianfan.baidubce.com/v2/models",
  },
  models: [
    { id: "deepseek-v4-pro", name: "DeepSeek V4 Pro", contextLength: 1048576 },
    { id: "deepseek-v4-flash", name: "DeepSeek V4 Flash", contextLength: 1048576 },
    { id: "glm-5.2", name: "GLM 5.2", contextLength: 512000 },
    { id: "glm-5.1", name: "GLM 5.1", contextLength: 198000 },
    { id: "kimi-k2.6", name: "Kimi K2.6", contextLength: 262144 },
    { id: "qwen3.5-397b-a17b", name: "Qwen 3.5 397B A17B", contextLength: 262144 },
    { id: "qwen3.5-27b", name: "Qwen 3.5 27B", contextLength: 262144 },
  ],
};
```
