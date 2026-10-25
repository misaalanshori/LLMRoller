# V1M System One (`v1m`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `v1m`
- **Display Name**: `V1M`
- **Category**: `apikey`
- **Service Kinds**: `["llm"]`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Website**: `https://v1m.ir`

---

## 2. Endpoints & Authentication

| Parameter | Value |
|---|---|
| **Base URL** | `https://v1m.ir/v1/systemone` |
| **Auth Header** | `Authorization: Bearer <apiKey>` |
| **Wire Format** | `"openai"` |

---

## 3. Configuration (`open-sse/config/providerRegistry.js` & `providerModels.js`)

```javascript
v1m: {
  baseUrl: "https://v1m.ir/v1/systemone",
  format: "openai",
  authType: "apikey",
  authHeader: "bearer",
  models: [
    { id: "rev-latest", name: "Rev Latest" },
    { id: "v1m-decision-engine", name: "V1M Decision Engine" },
  ],
},
```
