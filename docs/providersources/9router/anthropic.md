# Anthropic (`anthropic`) — Upstream Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `anthropic`
- **Alias**: `anthropic`
- **Display Name**: Anthropic
- **Category**: `apikey`
- **Service Kinds**: `["llm", "imageToText"]`
- **Website**: `https://console.anthropic.com`
- **API Key URL**: `https://console.anthropic.com/settings/keys`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Wire Format**: `claude` (Anthropic Messages API)

## 2. Endpoints
- **Messages Endpoint**: `POST https://api.anthropic.com/v1/messages`
- **Token Counting Endpoint** (per `ENDPOINT_DEFAULTS.claude`): `/messages/count_tokens`

## 3. Authentication & Headers
- **Authentication**: Uses `x-api-key: <apiKey>` (via `DefaultExecutor.resolveAuthDescriptor()`, which selects `{ combined: true, header: "x-api-key", scheme: "raw", anthropicVersion: true }` when `this.config.format === "claude"`).
- **Default Headers**:
  - `Content-Type: application/json`
  - `x-api-key: <apiKey>`
  - `anthropic-version: 2023-06-01`
  - `Anthropic-Beta: claude-code-20250219,interleaved-thinking-2025-05-14`
  - `Accept: text/event-stream` (when `stream: true`)
- **Beta Header Merging**: In `DefaultExecutor.buildHeaders()`, if the incoming client request includes `rawHeaders["anthropic-beta"]`, it is merged with `headers["Anthropic-Beta"]` via `mergeAnthropicBeta()` (deduplicating comma-separated beta flags).

## 4. Supported Models
- `claude-sonnet-4-20250514` ("Claude Sonnet 4")
- `claude-opus-4-20250514` ("Claude Opus 4")
- `claude-3-5-sonnet-20241022` ("Claude 3.5 Sonnet")

## 5. Source Implementation

### Registry Definition (`open-sse/providers/registry/anthropic.js`)
```javascript
export default {
  id: "anthropic",
  priority: 30,
  alias: "anthropic",
  display: {
    name: "Anthropic",
    icon: "smart_toy",
    color: "#D97757",
    textIcon: "AN",
    website: "https://console.anthropic.com",
    notice: {
      apiKeyUrl: "https://console.anthropic.com/settings/keys",
    },
  },
  category: "apikey",
  transport: {
    baseUrl: "https://api.anthropic.com/v1/messages",
    format: "claude",
    headers: {
      "anthropic-version": "2023-06-01",
      "Anthropic-Beta": "claude-code-20250219,interleaved-thinking-2025-05-14",
    },
  },
  models: [
    { id: "claude-sonnet-4-20250514", name: "Claude Sonnet 4" },
    { id: "claude-opus-4-20250514", name: "Claude Opus 4" },
    { id: "claude-3-5-sonnet-20241022", name: "Claude 3.5 Sonnet" },
  ],
  serviceKinds: ["llm","imageToText"],
};
```

### Auth & Header Logic in `DefaultExecutor` (`open-sse/executors/default.js`)
```javascript
  resolveAuthDescriptor() {
    if (this.provider?.startsWith?.("anthropic-compatible-")) {
      return { apiKey: { header: "x-api-key", scheme: "raw" }, oauth: { header: "Authorization", scheme: "bearer" }, anthropicVersion: true };
    }
    if (this.config?.format === "claude") {
      return { ...XAPIKEY, anthropicVersion: true };
    }
    return BEARER;
  }
```
