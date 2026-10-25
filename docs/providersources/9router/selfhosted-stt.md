# Self-hosted STT (`selfhosted-stt`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `selfhosted-stt`
- **Display Name**: `Self-hosted STT`
- **Alias**: `shstt`
- **Category**: `apikey`
- **Service Kinds**: `["audio"]` (`subKind: "stt"`)

---

## 2. Endpoints & Format

| Parameter | Value |
|---|---|
| **Default Base URL** | `http://localhost:8080/v1/audio/transcriptions` (overridable via `providerSpecificData.baseUrl`) |
| **Format** | `"openai"` (`multipart/form-data` with `file`, `model`, `language?`, `prompt?`, `response_format?`, `temperature?`) |
| **Auth Method** | Optional `Authorization: Bearer <apiKey>` |
| **Model** | `whisper-1` (`Self-hosted Whisper`) |

---

## 3. Implementation (`open-sse/config/audioRegistry.js` & `src/app/v1/audio/transcriptions/route.js`)

```javascript
"selfhosted-stt": {
  id: "selfhosted-stt",
  name: "Self-hosted STT",
  baseUrl: "http://localhost:8080/v1/audio/transcriptions",
  authType: "apikey",
  authHeader: "bearer",
  format: "openai",
  models: [
    { id: "whisper-1", name: "Self-hosted Whisper" },
  ],
},
```

```javascript
const targetUrl = providerId === "selfhosted-stt" && credentials.providerSpecificData?.baseUrl
  ? credentials.providerSpecificData.baseUrl
  : providerConfig.baseUrl;
```
