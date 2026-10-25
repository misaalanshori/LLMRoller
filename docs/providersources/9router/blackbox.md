# Blackbox AI (`blackbox`) — Upstream Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `blackbox`
- **Alias**: `blackbox`
- **Aliases**: `["bb"]`
- **UI Alias**: `bb`
- **Display Name**: Blackbox AI
- **Category**: `apikey`
- **Service Kinds**: `["llm"]`
- **Website**: `https://blackbox.ai`
- **API Key URL**: `https://www.blackbox.ai/api-management`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Wire Format**: `openai` (OpenAI Chat Completions compatible)

## 2. Endpoints & Thinking Configuration
- **Chat Completions Endpoint**: `POST https://api.blackbox.ai/v1/chat/completions`
- **Thinking Format**: `transport.thinkingFormat = "openai"` (uses OpenAI-style `reasoning_effort` with options `["auto", "none", "low", "medium", "high", "xhigh"]`, `defaultMode: "auto"`).

## 3. Authentication & Headers
- **Authentication**: Bearer token (`Authorization: Bearer <apiKey>`)
- **Headers**:
  - `Content-Type: application/json`
  - `Authorization: Bearer <apiKey>`
  - `Accept: text/event-stream` (when streaming)

## 4. Supported Models & Upstream Model ID Mapping
Each client-facing `id` is mapped to a prefixed `upstreamModelId` (`blackboxai/<vendor>/<model>`):
- `claude-fable-5` -> `blackboxai/anthropic/claude-fable-5`
- `claude-opus-4.8` -> `blackboxai/anthropic/claude-opus-4.8`
- `claude-sonnet-4.6` -> `blackboxai/anthropic/claude-sonnet-4.6`
- `gpt-5.5` -> `blackboxai/openai/gpt-5.5`
- `gpt-5.4-pro` -> `blackboxai/openai/gpt-5.4-pro`
- `gpt-5.4` -> `blackboxai/openai/gpt-5.4`
- `gpt-5.3-codex` -> `blackboxai/openai/gpt-5.3-codex`
- `gpt-5.4-nano` -> `blackboxai/openai/gpt-5.4-nano`
- `deepseek-v4-flash` -> `blackboxai/deepseek/deepseek-v4-flash`
- `grok-4.3` -> `blackboxai/x-ai/grok-4.3`

## 5. Source Implementation (`open-sse/providers/registry/blackbox.js`)
```javascript
export default {
  id: "blackbox",
  priority: 50,
  alias: "blackbox",
  aliases: [
    "bb",
  ],
  uiAlias: "bb",
  display: {
    name: "Blackbox AI",
    icon: "smart_toy",
    color: "#5B5FEF",
    textIcon: "BB",
    website: "https://blackbox.ai",
    notice: {
      apiKeyUrl: "https://www.blackbox.ai/api-management",
    },
  },
  category: "apikey",
  serviceKinds: ["llm"],
  thinkingConfig: {
    options: ["auto", "none", "low", "medium", "high", "xhigh"],
    defaultMode: "auto",
  },
  transport: {
    baseUrl: "https://api.blackbox.ai/v1/chat/completions",
    thinkingFormat: "openai",
  },
  models: [
    { id: "claude-fable-5",    name: "Claude Fable 5",    upstreamModelId: "blackboxai/anthropic/claude-fable-5" },
    { id: "claude-opus-4.8",   name: "Claude Opus 4.8",  upstreamModelId: "blackboxai/anthropic/claude-opus-4.8" },
    { id: "claude-sonnet-4.6", name: "Claude Sonnet 4.6", upstreamModelId: "blackboxai/anthropic/claude-sonnet-4.6" },
    { id: "gpt-5.5",           name: "GPT-5.5",           upstreamModelId: "blackboxai/openai/gpt-5.5" },
    { id: "gpt-5.4-pro",       name: "GPT-5.4 Pro",       upstreamModelId: "blackboxai/openai/gpt-5.4-pro" },
    { id: "gpt-5.4",           name: "GPT-5.4",           upstreamModelId: "blackboxai/openai/gpt-5.4" },
    { id: "gpt-5.3-codex",     name: "GPT-5.3 Codex",     upstreamModelId: "blackboxai/openai/gpt-5.3-codex" },
    { id: "gpt-5.4-nano",      name: "GPT-5.4 Nano",      upstreamModelId: "blackboxai/openai/gpt-5.4-nano" },
    { id: "deepseek-v4-flash", name: "DeepSeek V4 Flash", upstreamModelId: "blackboxai/deepseek/deepseek-v4-flash" },
    { id: "grok-4.3",          name: "Grok 4.3",          upstreamModelId: "blackboxai/x-ai/grok-4.3" },
  ],
};
```
