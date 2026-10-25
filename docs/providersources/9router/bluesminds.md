# BluesMinds (`bluesminds`) — Upstream Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `bluesminds`
- **Alias**: `bm`
- **Aliases**: `["blue-sminds"]`
- **UI Alias**: `bm`
- **Display Name**: BluesMinds
- **Hidden Flag**: `hidden: true`
- **Category**: `apikey`
- **Authentication Type**: `apikey`
- **Website**: `https://bluesminds.com`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Wire Format**: `openai` (OpenAI Chat Completions compatible)

## 2. Endpoints
- **Chat Completions Endpoint**: `POST https://api.bluesminds.com/v1/chat/completions`
- **Validation / Models Endpoint**: `GET https://api.bluesminds.com/v1/models`

## 3. Authentication & Headers
- **Authentication**: Bearer token (`Authorization: Bearer <apiKey>`)
- **Headers**:
  - `Content-Type: application/json`
  - `Authorization: Bearer <apiKey>`
  - `Accept: text/event-stream` (when streaming)

## 4. Supported Models
- `gpt-4.1` ("GPT-4.1", `contextLength: 1048576`)
- `gpt-4.1-mini` ("GPT-4.1 Mini", `contextLength: 1048576`)
- `gpt-4.1-nano` ("GPT-4.1 Nano", `contextLength: 1048576`)
- `claude-sonnet-4-5` ("Claude Sonnet 4.5", `contextLength: 200000`)
- `claude-haiku-4-5` ("Claude Haiku 4.5", `contextLength: 200000`)
- `gemini-2.0-flash` ("Gemini 2.0 Flash", `contextLength: 1048576`)
- `gemini-2.0-flash-exp` ("Gemini 2.0 Flash (Exp)", `contextLength: 1048576`)
- `qwen-turbo` ("Qwen Turbo", `contextLength: 1000000`)
- `kimi-k2` ("Kimi K2", `contextLength: 262144`)
- `kimi-k2-thinking` ("Kimi K2 Thinking", `contextLength: 262144`)
- `glm-4.7` ("GLM 4.7", `contextLength: 204800`)
- `minimax-m2.5` ("MiniMax M2.5", `contextLength: 204800`)
- `claude-opus-4-5` ("Claude Opus 4.5 (VIP)", `contextLength: 200000`)
- `gemini-2.5-pro` ("Gemini 2.5 Pro (VIP)", `contextLength: 1048576`)

## 5. Source Implementation (`open-sse/providers/registry/bluesminds.js`)
```javascript
export default {
  id: "bluesminds",
  alias: "bm",
  aliases: ["blue-sminds"],
  uiAlias: "bm",
  hidden: true,
  display: {
    name: "BluesMinds",
    icon: "psychology",
    color: "#2563EB",
    textIcon: "BM",
    website: "https://bluesminds.com",
    notice: { apiKeyUrl: "https://bluesminds.com" },
  },
  category: "apikey",
  authType: "apikey",
  authModes: ["apikey"],
  transport: {
    baseUrl: "https://api.bluesminds.com/v1/chat/completions",
    validateUrl: "https://api.bluesminds.com/v1/models",
  },
  models: [
    { id: "gpt-4.1", name: "GPT-4.1", contextLength: 1048576 },
    { id: "gpt-4.1-mini", name: "GPT-4.1 Mini", contextLength: 1048576 },
    { id: "gpt-4.1-nano", name: "GPT-4.1 Nano", contextLength: 1048576 },
    { id: "claude-sonnet-4-5", name: "Claude Sonnet 4.5", contextLength: 200000 },
    { id: "claude-haiku-4-5", name: "Claude Haiku 4.5", contextLength: 200000 },
    { id: "gemini-2.0-flash", name: "Gemini 2.0 Flash", contextLength: 1048576 },
    { id: "gemini-2.0-flash-exp", name: "Gemini 2.0 Flash (Exp)", contextLength: 1048576 },
    { id: "qwen-turbo", name: "Qwen Turbo", contextLength: 1000000 },
    { id: "kimi-k2", name: "Kimi K2", contextLength: 262144 },
    { id: "kimi-k2-thinking", name: "Kimi K2 Thinking", contextLength: 262144 },
    { id: "glm-4.7", name: "GLM 4.7", contextLength: 204800 },
    { id: "minimax-m2.5", name: "MiniMax M2.5", contextLength: 204800 },
    { id: "claude-opus-4-5", name: "Claude Opus 4.5 (VIP)", contextLength: 200000 },
    { id: "gemini-2.5-pro", name: "Gemini 2.5 Pro (VIP)", contextLength: 1048576 },
  ],
};
```
