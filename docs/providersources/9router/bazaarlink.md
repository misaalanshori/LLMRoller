# Bazaarlink (`bazaarlink`) — Upstream Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `bazaarlink`
- **Alias**: `bzl`
- **Aliases**: `["bazaar-link"]`
- **UI Alias**: `bzl`
- **Display Name**: Bazaarlink
- **Category**: `freeTier`
- **Authentication Type**: `apikey`
- **Website**: `https://bazaarlink.ai`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Wire Format**: `openai` (OpenAI Chat Completions compatible)

## 2. Endpoints
- **Chat Completions Endpoint**: `POST https://bazaarlink.ai/api/v1/chat/completions`
- **Validation / Models Endpoint**: `GET https://bazaarlink.ai/api/v1/models`

## 3. Authentication & Headers
- **Authentication**: Bearer token (`Authorization: Bearer <apiKey>`)
- **Headers**:
  - `Content-Type: application/json`
  - `Authorization: Bearer <apiKey>`
  - `Accept: text/event-stream` (when streaming)

## 4. Supported Models
- `auto:free` ("Auto Free (Zero Cost)")
- `claude-opus-4.7` ("Claude Opus 4.7", `contextLength: 1000000`)
- `claude-sonnet-4.6` ("Claude Sonnet 4.6", `contextLength: 1000000`)
- `claude-haiku-4.5` ("Claude Haiku 4.5", `contextLength: 200000`)
- `gpt-5.5` ("GPT-5.5", `contextLength: 1050000`)
- `gpt-5.4` ("GPT-5.4", `contextLength: 1050000`)
- `gpt-5.4-mini` ("GPT-5.4 Mini", `contextLength: 400000`)
- `gpt-5.4-nano` ("GPT-5.4 Nano", `contextLength: 400000`)
- `grok-4.3` ("Grok 4.3", `contextLength: 1000000`)
- `grok-4.20` ("Grok 4.20", `contextLength: 2000000`)
- `gemini-3.1-pro-preview` ("Gemini 3.1 Pro", `contextLength: 1048576`)
- `gemini-3-flash-preview` ("Gemini 3 Flash", `contextLength: 1048576`)
- `gemini-3.1-flash-lite-preview` ("Gemini 3.1 Flash Lite", `contextLength: 1048576`)
- `kimi-k2.6` ("Kimi K2.6", `contextLength: 262144`)
- `kimi-k2.5` ("Kimi K2.5", `contextLength: 262144`)
- `glm-5.1` ("GLM 5.1", `contextLength: 204800`)
- `glm-5` ("GLM 5", `contextLength: 204800`)
- `mimo-v2.5-pro` ("MiMo-V2.5-Pro", `contextLength: 1050000`)
- `mimo-v2.5` ("MiMo-V2.5", `contextLength: 1050000`)
- `minimax-m3` ("MiniMax M3", `contextLength: 1048576`)
- `minimax-m2.7` ("MiniMax M2.7", `contextLength: 204800`)
- `minimax-m2.5` ("MiniMax M2.5", `contextLength: 204800`)
- `qwen3.6-plus` ("Qwen 3.6 Plus", `contextLength: 1000000`)
- `nemotron-3-super-120b-a12b` ("Nemotron 3 Super", `contextLength: 1000000`)

## 5. Source Implementation (`open-sse/providers/registry/bazaarlink.js`)
```javascript
export default {
  id: "bazaarlink",
  alias: "bzl",
  aliases: ["bazaar-link"],
  uiAlias: "bzl",
  category: "freeTier",
  authType: "apikey",
  authModes: ["apikey"],
  display: {
    name: "Bazaarlink",
    icon: "storefront",
    color: "#DC2626",
    textIcon: "BZ",
    website: "https://bazaarlink.ai",
    notice: { apiKeyUrl: "https://bazaarlink.ai" },
  },
  transport: {
    baseUrl: "https://bazaarlink.ai/api/v1/chat/completions",
    validateUrl: "https://bazaarlink.ai/api/v1/models",
  },
  models: [
    { id: "auto:free", name: "Auto Free (Zero Cost)" },
    { id: "claude-opus-4.7", name: "Claude Opus 4.7", contextLength: 1000000 },
    { id: "claude-sonnet-4.6", name: "Claude Sonnet 4.6", contextLength: 1000000 },
    { id: "claude-haiku-4.5", name: "Claude Haiku 4.5", contextLength: 200000 },
    { id: "gpt-5.5", name: "GPT-5.5", contextLength: 1050000 },
    { id: "gpt-5.4", name: "GPT-5.4", contextLength: 1050000 },
    { id: "gpt-5.4-mini", name: "GPT-5.4 Mini", contextLength: 400000 },
    { id: "gpt-5.4-nano", name: "GPT-5.4 Nano", contextLength: 400000 },
    { id: "grok-4.3", name: "Grok 4.3", contextLength: 1000000 },
    { id: "grok-4.20", name: "Grok 4.20", contextLength: 2000000 },
    { id: "gemini-3.1-pro-preview", name: "Gemini 3.1 Pro", contextLength: 1048576 },
    { id: "gemini-3-flash-preview", name: "Gemini 3 Flash", contextLength: 1048576 },
    { id: "gemini-3.1-flash-lite-preview", name: "Gemini 3.1 Flash Lite", contextLength: 1048576 },
    { id: "kimi-k2.6", name: "Kimi K2.6", contextLength: 262144 },
    { id: "kimi-k2.5", name: "Kimi K2.5", contextLength: 262144 },
    { id: "glm-5.1", name: "GLM 5.1", contextLength: 204800 },
    { id: "glm-5", name: "GLM 5", contextLength: 204800 },
    { id: "mimo-v2.5-pro", name: "MiMo-V2.5-Pro", contextLength: 1050000 },
    { id: "mimo-v2.5", name: "MiMo-V2.5", contextLength: 1050000 },
    { id: "minimax-m3", name: "MiniMax M3", contextLength: 1048576 },
    { id: "minimax-m2.7", name: "MiniMax M2.7", contextLength: 204800 },
    { id: "minimax-m2.5", name: "MiniMax M2.5", contextLength: 204800 },
    { id: "qwen3.6-plus", name: "Qwen 3.6 Plus", contextLength: 1000000 },
    { id: "nemotron-3-super-120b-a12b", name: "Nemotron 3 Super", contextLength: 1000000 },
  ],
};
```
