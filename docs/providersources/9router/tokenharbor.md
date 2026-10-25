# TokenHarbor (`tokenharbor`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `tokenharbor`
- **Display Name**: `TokenHarbor`
- **Category**: `apikey`
- **Service Kinds**: `["llm"]`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Website**: `https://tokenharbor.ai`

---

## 2. Endpoints & Authentication

| Parameter | Value |
|---|---|
| **Base URL** | `https://tokenharbor.ai/v1/chat/completions` |
| **Models / Validation URL** | `https://tokenharbor.ai/v1/models` (`modelsFetcher: "https://tokenharbor.ai/v1/models"`) |
| **Auth Header** | `Authorization: Bearer <apiKey>` |
| **Wire Format** | `"openai"` (`passthroughModels: true`) |

---

## 3. Configuration (`open-sse/config/providerRegistry.js` & `providerModels.js`)

```javascript
tokenharbor: {
  baseUrl: "https://tokenharbor.ai/v1/chat/completions",
  modelsUrl: "https://tokenharbor.ai/v1/models",
  format: "openai",
  authType: "apikey",
  authHeader: "bearer",
  modelsFetcher: "https://tokenharbor.ai/v1/models",
  models: [
    { id: "claude-opus-5.5", name: "Claude Opus 5.5" },
    { id: "claude-sonnet-5", name: "Claude Sonnet 5" },
    { id: "gpt-6-astra", name: "GPT 6 Astra" },
    { id: "gpt-6-sol", name: "GPT 6 Sol" },
    { id: "deepseek-v4.1-flash:free", name: "DeepSeek V4.1 Flash (Free)" },
    { id: "grok-4.7", name: "Grok 4.7" },
  ],
},
```
