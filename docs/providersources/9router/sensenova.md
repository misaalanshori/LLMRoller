# SenseNova (`sensenova`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `sensenova`
- **Display Name**: `SenseNova`
- **Category**: `apikey`
- **Service Kinds**: `["llm", "image"]`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`) + `openaiImage.js` (`open-sse/handlers/imageProviders/openaiImage.js`)
- **Website**: `https://platform.sensenova.cn`

---

## 2. Endpoints & Authentication

| Service | Endpoint |
|---|---|
| **Chat Completions** | `POST https://token.sensenova.cn/v1/chat/completions` |
| **Image Generation** | `POST https://token.sensenova.cn/v1/images/generations` |
| **API Key Validation** | `GET https://token.sensenova.cn/v1/models` |
| **Authentication** | `Authorization: Bearer <apiKey>` |

---

## 3. Configuration (`open-sse/config/providerRegistry.js` & `imageRegistry.js`)

```javascript
sensenova: {
  baseUrl: "https://token.sensenova.cn/v1/chat/completions",
  modelsUrl: "https://token.sensenova.cn/v1/models",
  format: "openai",
  authType: "apikey",
  authHeader: "bearer",
  models: [
    { id: "sensenova-6.8-flash-lite", name: "SenseNova 6.8 Flash Lite" },
    { id: "deepseek-v4-flash", name: "DeepSeek V4 Flash" },
    { id: "deepseek-flash", name: "DeepSeek Flash" },
    { id: "glm-5.2", name: "GLM 5.2" },
    { id: "kimi-k3", name: "Kimi K3" },
  ],
},
```

### Image Generation Configuration (`open-sse/config/imageRegistry.js`)

```javascript
sensenova: {
  id: "sensenova",
  name: "SenseNova",
  baseUrl: "https://token.sensenova.cn/v1/images/generations",
  authType: "apikey",
  authHeader: "bearer",
  format: "openai",
  bodyFields: ["model", "prompt", "n", "size", "output_format", "response_format", "watermark", "prompt_extend"],
  models: [
    { id: "sensenova-u1.5-lite", name: "SenseNova U1.5 Lite", supportedSizes: ["1024x1024", "1024x768", "768x1024", "1280x720", "720x1280"] },
    { id: "sensenova-u1.5-fast", name: "SenseNova U1.5 Fast", supportedSizes: ["1024x1024", "1024x768", "768x1024", "1280x720", "720x1280"] },
  ],
},
```
