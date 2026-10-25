# Tencent Hunyuan (`tencent`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `tencent`
- **Display Name**: `Tencent Hunyuan`
- **Category**: `apikey`
- **Service Kinds**: `["llm"]`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Website**: `https://cloud.tencent.com/product/hunyuan`

---

## 2. Endpoints & Authentication

| Parameter | Value |
|---|---|
| **Base URL** | `https://api.hunyuan.cloud.tencent.com/v1/chat/completions` |
| **Models / Validation URL** | `https://api.hunyuan.cloud.tencent.com/v1/models` |
| **Auth Header** | `Authorization: Bearer <apiKey>` |
| **Wire Format** | `"openai"` |

---

## 3. Configuration (`open-sse/config/providerRegistry.js` & `providerModels.js`)

```javascript
tencent: {
  baseUrl: "https://api.hunyuan.cloud.tencent.com/v1/chat/completions",
  modelsUrl: "https://api.hunyuan.cloud.tencent.com/v1/models",
  format: "openai",
  authType: "apikey",
  authHeader: "bearer",
  models: [
    { id: "hunyuan-turbos-latest", name: "Hunyuan TurboS" },
    { id: "hunyuan-t1-latest", name: "Hunyuan T1" },
  ],
},
```
