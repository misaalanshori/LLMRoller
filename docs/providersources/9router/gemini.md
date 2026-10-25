# Google Gemini (`gemini`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `gemini`
- **Priority**: `50` (`hasFree: true`, `mediaPriority: 1`)
- **Alias**: `gemini`
- **Display Name**: Gemini (Google AI Studio / Generative Language API)
- **Category**: `freeTier`
- **Auth Type**: `apikey` (`authModes: ["apikey"]`, also supports OAuth Bearer fallback in `transport.auth`)
- **Service Kinds**: `["llm", "embedding", "image", "imageToText", "webSearch", "tts", "stt"]`
- **Website**: `https://ai.google.dev`
- **API Key URL**: `https://aistudio.google.com/app/apikey`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source Files**:
  - `open-sse/providers/registry/gemini.js`
  - `open-sse/executors/default.js` (`buildUrl` for `gemini`)
  - `open-sse/handlers/embeddingProviders/gemini.js`
  - `open-sse/handlers/imageProviders/gemini.js`
  - `open-sse/handlers/sttCore.js` (`transcribeGemini`)
  - `open-sse/handlers/geminiLiveStt.js` (`transcribeGeminiLive`)

---

## 2. Endpoints by Service Kind
1. **LLM Chat & Vision (`DefaultExecutor.buildUrl`)**:
   - Streaming: `POST https://generativelanguage.googleapis.com/v1beta/models/<model>:streamGenerateContent?alt=sse`
   - Non-Streaming: `POST https://generativelanguage.googleapis.com/v1beta/models/<model>:generateContent`
   - Wire Format: `"gemini"`
   - Auth Headers: `x-goog-api-key: <apiKey>` (when using API key) or `Authorization: Bearer <accessToken>` (when using OAuth).
2. **Embeddings (`open-sse/handlers/embeddingProviders/gemini.js`)**:
   - Single Input: `POST https://generativelanguage.googleapis.com/v1beta/models/<model>:embedContent?key=<apiKey>`
     - Body: `{ model: "models/<model>", content: { parts: [{ text }] }, outputDimensionality? }`
   - Array Input: `POST https://generativelanguage.googleapis.com/v1beta/models/<model>:batchEmbedContents?key=<apiKey>`
     - Body: `{ requests: [{ model: "models/<model>", content: { parts: [{ text }] }, outputDimensionality? }] }`
3. **Image Generation / Editing (`open-sse/handlers/imageProviders/gemini.js`)**:
   - `POST https://generativelanguage.googleapis.com/v1beta/models/<model>:generateContent?key=<apiKey>`
   - Body: `{ contents: [{ parts: [inlineData?, { text: prompt }] }], generationConfig: { responseModalities: ["TEXT", "IMAGE"] } }`
4. **REST Speech-to-Text (`transcribeGemini` in `open-sse/handlers/sttCore.js`)**:
   - `POST https://generativelanguage.googleapis.com/v1beta/models/<model>:generateContent?key=<apiKey>`
   - Sends `{ contents: [{ parts: [{ text: promptText }, { inline_data: { mime_type, data: base64Audio } }] }] }`.
5. **Realtime Live WebSocket Speech-to-Text (`open-sse/handlers/geminiLiveStt.js`)**:
   - Used for models with `transport: "gemini-live"` (`gemini-2.5-flash-native-audio-preview-09-17`).
   - WebSocket Endpoint: `wss://generativelanguage.googleapis.com/ws/api/v1beta/models/<model>:bidiGenerateContent?key=<apiKey>`
   - Sends `{ setup: { model: "models/<model>", generationConfig: { responseModalities: ["TEXT"], inputAudioTranscription: {} }, systemInstruction: { parts: [{ text }] } } }`, waits for `serverContent.setupComplete`, streams 16 KiB base64 chunks via `realtimeInput.mediaChunks`, and accumulates `serverContent.inputTranscription.text` until `serverContent.turnComplete` (with automatic `goAway` socket rotation).
6. **Text-to-Speech (`ttsConfig`)**:
   - `POST https://generativelanguage.googleapis.com/v1beta/models/<model>:generateContent?key=<apiKey>` (`format: "gemini-tts"`)
7. **Web Search via Chat (`searchViaChat`)**:
   - `POST https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent` (with Google Search grounding tool).

---

## 3. Supported Models
| Model ID | Display Name | Kind | Extra Metadata |
|---|---|---|---|
| `gemini-4-argon` | Gemini 4 Argon | `llm` | — |
| `gemini-3.8-flash` | Gemini 3.8 Flash | `llm` | — |
| `gemini-3.7-flash` | Gemini 3.7 Flash | `llm` | — |
| `gemini-3.6-flash` | Gemini 3.6 Flash | `llm` | — |
| `gemini-3.5-flash-lite` | Gemini 3.5 Flash Lite | `llm` | — |
| `gemini-3.1-pro-preview` | Gemini 3.1 Pro Preview | `llm` | — |
| `gemini-3.1-flash-lite-preview` | Gemini 3.1 Flash Lite Preview | `llm` | — |
| `gemini-3-flash-preview` | Gemini 3 Flash Preview | `llm` | — |
| `gemini-2.5-pro` | Gemini 2.5 Pro | `llm` / `stt` | — |
| `gemini-2.5-flash` | Gemini 2.5 Flash | `llm` / `stt` | — |
| `gemini-2.5-flash-lite` | Gemini 2.5 Flash Lite | `llm` / `stt` | — |
| `gemini-2.0-flash` | Gemini 2.0 Flash | `stt` | `params: ["language", "prompt"]` |
| `gemini-2.5-flash-native-audio-preview-09-17` | Gemini Live Transcription (Realtime) | `stt` | `transport: "gemini-live"` |
| `gemma-4-31b-it` | Gemma 4 31B IT | `llm` | — |
| `gemini-embedding-2-preview` | Gemini Embedding 2 Preview | `embedding` | — |
| `gemini-embedding-001` | Gemini Embedding 001 | `embedding` | — |
| `text-embedding-005` | Text Embedding 005 | `embedding` | — |
| `text-embedding-004` | Text Embedding 004 (Legacy) | `embedding` | — |
| `embedding-001` | Embedding 001 | `embedding` | `dimensions: 768` |
| `gemini-3.1-flash-image-preview` | Gemini 3.1 Flash Image (Nano Banana 2) | `image` | — |
| `gemini-3-pro-image-preview` | Gemini 3 Pro Image (Nano Banana Pro) | `image` | — |
| `gemini-2.5-flash-image` | Gemini 2.5 Flash Image (Nano Banana) | `image` | — |
| `gemini-3.1-flash-tts-preview` | Gemini 3.1 Flash TTS | `tts` | — |
| `gemini-2.5-flash-preview-tts` | Gemini 2.5 Flash TTS | `tts` | — |
| `gemini-2.5-pro-preview-tts` | Gemini 2.5 Pro TTS | `tts` | — |

---

## 4. Verbatim Source Code Snippets

### `open-sse/providers/registry/gemini.js`
```javascript
import { GOOGLE_OAUTH_CLIENT } from "../shared.js";

export default {
  id: "gemini",
  priority: 50,
  hasFree: true,
  alias: "gemini",
  category: "freeTier",
  authType: "apikey",
  authModes: ["apikey"],
  mediaPriority: 1,
  transport: {
    baseUrl: "https://generativelanguage.googleapis.com/v1beta/models",
    format: "gemini",
    clientId: "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com",
    clientSecret: "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",
    auth: {
      apiKey: {
        header: "x-goog-api-key",
        scheme: "raw",
      },
      oauth: {
        header: "Authorization",
        scheme: "bearer",
      },
    },
  },
  serviceKinds: ["llm","embedding","image","imageToText","webSearch","tts","stt"],
  ttsConfig: {
    baseUrl: "https://generativelanguage.googleapis.com/v1beta/models",
    authType: "apikey",
    authHeader: "key",
    format: "gemini-tts",
  },
  sttConfig: {
    baseUrl: "https://generativelanguage.googleapis.com/v1beta/models",
    authType: "apikey",
    authHeader: "key",
    format: "gemini-stt",
  },
  embeddingConfig: { baseUrl: "https://generativelanguage.googleapis.com/v1beta/models", authType: "apikey", authHeader: "key" },
  imageConfig: { baseUrl: "https://generativelanguage.googleapis.com/v1beta/models" },
  searchViaChat: {
    defaultModel: "gemini-2.5-flash",
    endpoint: "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent",
    pricingUrl: "https://ai.google.dev/pricing",
    freeTier: "Free tier: 15 RPM, 1M tokens/day on gemini-2.5-flash via AI Studio.",
  },
};
```
