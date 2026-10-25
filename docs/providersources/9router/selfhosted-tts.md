# Self-hosted TTS (`selfhosted-tts`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `selfhosted-tts`
- **Display Name**: `Self-hosted TTS`
- **Alias**: `shtts`
- **Category**: `apikey` (`noAuth: true`)
- **Service Kinds**: `["audio"]` (`subKind: "tts"`)
- **Description**: `Kokoro-FastAPI, OpenedAI Speech, LocalAI, or any OpenAI-compatible TTS server`

---

## 2. Endpoints & Configuration

| Parameter | Value |
|---|---|
| **Default Base URL** | `http://localhost:8880` (`POST ${baseUrl}/v1/audio/speech`) |
| **Auth Method** | Optional `Authorization: Bearer <apiKey>` (`noAuth: true` by default) |
| **Format** | `"openai-speech"` (`buildOpenAISpeechRequest`) |
| **Default Model** | `kokoro` |
| **Supported Models** | `kokoro`, `tts-1`, `tts-1-hd` |
| **Voices** | `af_heart`, `af_bella`, `af_nicole`, `af_sarah`, `af_sky`, `am_adam`, `am_michael`, `bf_emma`, `bm_george`, `alloy`, `nova` |

---

## 3. Implementation (`open-sse/config/ttsRegistry.js` & `open-sse/services/ttsFormats/genericFormats.js`)

```javascript
"selfhosted-tts": {
  id: "selfhosted-tts",
  name: "Self-hosted TTS",
  format: "openai-speech",
  baseUrl: "http://localhost:8880",
  authType: "none",
  noAuth: true,
  defaultModel: "kokoro",
  models: [
    { id: "kokoro", name: "Kokoro (Kokoro-FastAPI)" },
    { id: "tts-1", name: "TTS-1 (OpenedAI Speech / LocalAI)" },
    { id: "tts-1-hd", name: "TTS-1 HD" },
  ],
},
```

```javascript
export function buildOpenAISpeechRequest({ text, voiceId, model, speed = 1.0, responseFormat = "mp3", baseUrl }, apiKey) {
  const base = (baseUrl || "http://localhost:8880").replace(/\/$/, "");
  const headers = { "Content-Type": "application/json" };
  if (apiKey) headers["Authorization"] = `Bearer ${apiKey}`;
  return {
    url: `${base}/v1/audio/speech`,
    headers,
    body: JSON.stringify({ model: model || "kokoro", input: text, voice: voiceId || "af_heart", speed, response_format: responseFormat }),
  };
}
```
