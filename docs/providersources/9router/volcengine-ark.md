# Volcengine Ark (`volcengine-ark`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `volcengine-ark`
- **Display Name**: `Volcengine Ark`
- **Alias**: `ark`
- **Category**: `apikey`
- **Service Kinds**: `["llm"]`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Website**: `https://ark.cn-beijing.volces.com`

---

## 2. Endpoints & Authentication

| Parameter | Value |
|---|---|
| **Base URL** | `https://ark.cn-beijing.volces.com/api/coding/v3/chat/completions` |
| **Auth Header** | `Authorization: Bearer <apiKey>` |
| **Wire Format** | `"openai"` |

---

## 3. Registry Definition (`open-sse/providers/registry/volcengine-ark.js`)

```javascript
export default {
  id: "volcengine-ark",
  priority: 270,
  alias: "volcengine-ark",
  aliases: ["ark"],
  uiAlias: "ark",
  category: "apikey",
  transport: {
    baseUrl: "https://ark.cn-beijing.volces.com/api/coding/v3/chat/completions",
    headers: {},
  },
  models: [
    { id: "Doubao-Seed-2.0-Code", name: "Doubao-Seed-2.0-Code" },
    { id: "Doubao-Seed-2.0-pro", name: "Doubao-Seed-2.0-pro" },
    { id: "Doubao-Seed-2.0-lite", name: "Doubao-Seed-2.0-lite" },
    { id: "Doubao-Seed-Code", name: "Doubao-Seed-Code" },
    { id: "DeepSeek-V4-Flash", name: "DeepSeek-V4-Flash" },
    { id: "DeepSeek-V4-Pro", name: "DeepSeek-V4-Pro" },
    { id: "GLM-5.1", name: "GLM-5.1" },
    { id: "MiniMax-M2.7", name: "MiniMax-M2.7" },
    { id: "Kimi-K2.6", name: "Kimi-K2.6" },
  ],
};
```
