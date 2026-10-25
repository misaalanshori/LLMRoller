# Groq (`groq`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `groq`
- **Priority**: `60` (`hasFree: true`)
- **Alias**: `groq`
- **Display Name**: Groq
- **Category**: `apikey`
- **Service Kinds**: `["llm", "imageToText", "stt"]`
- **Website**: `https://groq.com`
- **API Key URL**: `https://console.groq.com/keys`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source Files**:
  - `open-sse/providers/registry/groq.js`
  - `open-sse/handlers/sttCore.js` (`transcribeOpenAICompatible`)

---

## 2. Endpoints by Service Kind
- **LLM & Vision Chat Completions**: `POST https://api.groq.com/openai/v1/chat/completions`
- **Models & Rate-Limit Usage Endpoint**: `GET https://api.groq.com/openai/v1/models` (reads `x-ratelimit-*` response headers)
- **Speech-to-Text (STT) Endpoint**: `POST https://api.groq.com/openai/v1/audio/transcriptions` (`format: "openai"`, multipart `FormData` with `file`, `model`, `language`, `prompt`, `response_format`, `temperature`)

---

## 3. Authentication & Headers
- **Authorization Header**: `Authorization: Bearer <apiKey>`
- **Content-Type**: `application/json` (LLM) or `multipart/form-data` (STT)

---

## 4. Supported Models
| Model ID | Display Name | Kind | Supported Params |
|---|---|---|---|
| `llama-3.3-70b-versatile` | Llama 3.3 70B | `llm` | — |
| `meta-llama/llama-4-maverick-17b-128e-instruct` | Llama 4 Maverick | `llm` | — |
| `qwen/qwen3-32b` | Qwen3 32B | `llm` | — |
| `openai/gpt-oss-120b` | GPT-OSS 120B | `llm` | — |
| `whisper-large-v3` | Whisper Large v3 | `stt` | `["language", "response_format", "temperature", "prompt"]` |
| `whisper-large-v3-turbo` | Whisper Large v3 Turbo | `stt` | `["language", "response_format", "temperature", "prompt"]` |
| `distil-whisper-large-v3-en` | Distil Whisper Large v3 EN | `stt` | `["language", "response_format", "temperature", "prompt"]` |

---

## 5. Verbatim Source Code

### `open-sse/providers/registry/groq.js`
```javascript
export default {
  id: "groq",
  priority: 60,
  hasFree: true,
  alias: "groq",
  display: {
    name: "Groq",
    icon: "speed",
    color: "#F55036",
    textIcon: "GQ",
    website: "https://groq.com",
    notice: {
      apiKeyUrl: "https://console.groq.com/keys",
    },
  },
  category: "apikey",
  transport: {
    baseUrl: "https://api.groq.com/openai/v1/chat/completions",
    validateUrl: "https://api.groq.com/openai/v1/models",
    usage: {
      url: "https://api.groq.com/openai/v1/models",
    },
  },
  models: [
    { id: "llama-3.3-70b-versatile", name: "Llama 3.3 70B" },
    { id: "meta-llama/llama-4-maverick-17b-128e-instruct", name: "Llama 4 Maverick" },
    { id: "qwen/qwen3-32b", name: "Qwen3 32B" },
    { id: "openai/gpt-oss-120b", name: "GPT-OSS 120B" },
    { id: "whisper-large-v3", name: "Whisper Large v3", params: ["language","response_format","temperature","prompt"], kind: "stt" },
    { id: "whisper-large-v3-turbo", name: "Whisper Large v3 Turbo", params: ["language","response_format","temperature","prompt"], kind: "stt" },
    { id: "distil-whisper-large-v3-en", name: "Distil Whisper Large v3 EN", params: ["language","response_format","temperature","prompt"], kind: "stt" },
  ],
  serviceKinds: ["llm","imageToText","stt"],
  sttConfig: {
    baseUrl: "https://api.groq.com/openai/v1/audio/transcriptions",
    authType: "apikey",
    authHeader: "bearer",
    format: "openai",
  },
  features: {
    usage: true,
    usageApikey: true,
  },
};
```
