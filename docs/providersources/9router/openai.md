# OpenAI (`openai`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `openai`
- **Priority**: `30`
- **Alias**: `openai`
- **Display Name**: OpenAI
- **Category**: `apikey`
- **Service Kinds**: `["llm", "embedding", "tts", "stt", "image", "imageToText", "webSearch"]`
- **Website**: `https://platform.openai.com`
- **API Key URL**: `https://platform.openai.com/api-keys`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source File**: `open-sse/providers/registry/openai.js`

---

## 2. Endpoints by Service Kind
1. **LLM & Vision Chat Completions**: `POST https://api.openai.com/v1/chat/completions` (`forceStream: true`)
2. **Embeddings (`embeddingConfig`)**: `POST https://api.openai.com/v1/embeddings`
3. **Text-to-Speech (`ttsConfig`)**: `POST https://api.openai.com/v1/audio/speech` (`defaultModel: "gpt-4o-mini-tts"`, `format: "openai"`)
4. **Speech-to-Text (`sttConfig`)**: `POST https://api.openai.com/v1/audio/transcriptions` (`format: "openai"`)
5. **Image Generation (`imageConfig`)**: `POST https://api.openai.com/v1/images/generations`
6. **Web Search via Chat (`searchViaChat`)**: `defaultModel: "gpt-4o-mini"`

---

## 3. Authentication & Thinking Configuration
- **Authentication**: `Authorization: Bearer <apiKey>`
- **Thinking Options**: `["auto", "none", "low", "medium", "high"]` (`defaultMode: "auto"`)

---

## 4. Supported Models
| Model ID | Display Name | Kind |
|---|---|---|
| `gpt-5.5`, `gpt-5.4`, `gpt-5.4-mini`, `gpt-5.4-nano`, `gpt-5.2`, `gpt-5.1`, `gpt-5`, `gpt-5-mini`, `gpt-5-nano` | GPT-5 Series | `llm` |
| `gpt-4o`, `gpt-4o-mini`, `gpt-4-turbo`, `gpt-4.1`, `gpt-4.1-mini`, `gpt-4.1-nano` | GPT-4 Series | `llm` |
| `o3`, `o3-mini`, `o3-pro`, `o4-mini`, `o1`, `o1-mini` | o-Series Reasoning | `llm` |
| `text-embedding-3-large`, `text-embedding-3-small`, `text-embedding-ada-002` | Embeddings | `embedding` |
| `tts-1`, `tts-1-hd`, `gpt-4o-mini-tts` | Text-to-Speech | `tts` |
| `whisper-1`, `gpt-4o-transcribe`, `gpt-4o-mini-transcribe` | Speech-to-Text | `stt` |
| `gpt-image-2.5`, `gpt-image-2.5-flare`, `gpt-image-2.5-sunburst`, `gpt-image-1`, `dall-e-3`, `dall-e-2` | Image Generation | `image` |

---

## 5. Verbatim Source Code

### `open-sse/providers/registry/openai.js`
```javascript
export default {
  id: "openai",
  priority: 30,
  alias: "openai",
  display: {
    name: "OpenAI",
    icon: "auto_awesome",
    color: "#10A37F",
    textIcon: "OA",
    website: "https://platform.openai.com",
    notice: {
      apiKeyUrl: "https://platform.openai.com/api-keys",
    },
  },
  category: "apikey",
  thinkingConfig: {
    options: [
      "auto",
      "none",
      "low",
      "medium",
      "high",
    ],
    defaultMode: "auto",
  },
  transport: {
    baseUrl: "https://api.openai.com/v1/chat/completions",
    forceStream: true,
  },
  serviceKinds: ["llm","embedding","tts","stt","image","imageToText","webSearch"],
  ttsConfig: {
    baseUrl: "https://api.openai.com/v1/audio/speech",
    authType: "apikey",
    authHeader: "bearer",
    format: "openai",
    defaultModel: "gpt-4o-mini-tts",
  },
  sttConfig: {
    baseUrl: "https://api.openai.com/v1/audio/transcriptions",
    authType: "apikey",
    authHeader: "bearer",
    format: "openai",
  },
  embeddingConfig: { baseUrl: "https://api.openai.com/v1/embeddings", authType: "apikey", authHeader: "bearer" },
  imageConfig: { baseUrl: "https://api.openai.com/v1/images/generations" },
  searchViaChat: { defaultModel: "gpt-4o-mini", pricingUrl: "https://openai.com/api/pricing" },
};
```
