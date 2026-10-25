# Alibaba Token Plan (`alitp-intl`) — Upstream Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `alitp-intl`
- **Alias**: `alitp-intl`
- **Display Name**: Alibaba Token Plan
- **Category**: `apikey`
- **Website**: `https://www.alibabacloud.com/campaign/ai-landing-page-token`
- **API Key URL**: `https://modelstudio.console.alibabacloud.com/?apiKey=1`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Wire Format**: `openai` (OpenAI Chat Completions compatible)

## 2. Endpoints & Quirks
- **Chat Completions Endpoint**: `POST https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1/chat/completions`
- **Regional & Surface Constraints**:
  - Dedicated to credit subscription keys on `token-plan.<region>.maas.aliyuncs.com`.
  - `ap-southeast-1` (Singapore) is the only region serving this plan (`eu-central-1` returns `IllegalEndpoint`).
  - The Anthropic surface (`/apps/anthropic/v1/messages`) is not authorized for this plan, so OpenAI-compatible mode (`/compatible-mode/v1/chat/completions`) is the only supported transport.
- **Transport Quirks**:
  - `preserveCacheControl: true` — preserves `cache_control` markers on message content parts.

## 3. Authentication & Headers
- **Authentication**: Bearer token (`Authorization: Bearer <apiKey>`)
- **Headers**:
  - `Content-Type: application/json`
  - `Authorization: Bearer <apiKey>`
  - `Accept: text/event-stream` (when streaming)

## 4. Supported Models
- `qwen3.8-max-preview` ("Qwen3.8 Max Preview")
- `qwen3.7-max` ("Qwen3.7 Max")
- `qwen3.7-plus` ("Qwen3.7 Plus")
- `qwen3.6-flash` ("Qwen3.6 Flash")
- `glm-5.2` ("GLM 5.2")
- `deepseek-v4-pro` ("DeepSeek V4 Pro")

## 5. Source Implementation (`open-sse/providers/registry/alitp-intl.js`)
```javascript
// Token Plan — credit subscription keys on token-plan.<region>.maas.aliyuncs.com.
// Fourth Alibaba key type: Coding Plan (alicode/alicode-intl) and Model Studio
// (alims-intl) both reject these keys, and they reject Model Studio keys back.
// Singapore is the only region that serves the plan; eu-central-1 answers
// IllegalEndpoint. The Anthropic surface (/apps/anthropic/v1/messages) is not
// authorized for this plan, so OpenAI-compatible mode is the only transport.
export default {
  id: "alitp-intl",
  priority: 11,
  alias: "alitp-intl",
  display: {
    name: "Alibaba Token Plan",
    icon: "cloud",
    color: "#FF6A00",
    textIcon: "ATP",
    website: "https://www.alibabacloud.com/campaign/ai-landing-page-token",
    notice: {
      apiKeyUrl: "https://modelstudio.console.alibabacloud.com/?apiKey=1",
    },
  },
  category: "apikey",
  transport: {
    baseUrl: "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1/chat/completions",
    headers: {},
    quirks: { preserveCacheControl: true },
  },
  models: [
    { id: "qwen3.8-max-preview", name: "Qwen3.8 Max Preview" },
    { id: "qwen3.7-max", name: "Qwen3.7 Max" },
    { id: "qwen3.7-plus", name: "Qwen3.7 Plus" },
    { id: "qwen3.6-flash", name: "Qwen3.6 Flash" },
    { id: "glm-5.2", name: "GLM 5.2" },
    { id: "deepseek-v4-pro", name: "DeepSeek V4 Pro" },
  ],
};
```
