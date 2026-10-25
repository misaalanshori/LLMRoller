# NVIDIA NIM (`nvidia`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `nvidia`
- **Priority**: `20` (`hasFree: true`)
- **Alias**: `nvidia`
- **Display Name**: NVIDIA NIM
- **Category**: `freeTier`
- **Auth Type**: `apikey` (`authModes: ["apikey"]`)
- **Service Kinds**: `["llm", "tts", "embedding"]` (plus `nvidia/parakeet-ctc-1.1b-asr` STT model handled via `nvidia-asr` in `sttCore.js`)
- **Website**: `https://developer.nvidia.com/nim`
- **API Key URL**: `https://build.nvidia.com/settings/api-keys`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source Files**:
  - `open-sse/providers/registry/nvidia.js`
  - `open-sse/handlers/ttsProviders/genericFormats.js` (`nvidia` TTS handler)
  - `open-sse/handlers/sttCore.js` (`transcribeNvidia`)

---

## 2. Endpoints by Service Kind
1. **LLM Chat Completions**: `POST https://integrate.api.nvidia.com/v1/chat/completions`
2. **Validation / Models Endpoint**: `GET https://integrate.api.nvidia.com/v1/models`
3. **Embeddings Endpoint**: `POST https://integrate.api.nvidia.com/v1/embeddings`
4. **Text-to-Speech (`ttsConfig` -> `nvidia` in `genericFormats.js`)**:
   - **Endpoint**: `POST https://integrate.api.nvidia.com/v1/audio/speech`
   - **Headers**: `Authorization: Bearer <apiKey>`, `Content-Type: application/json`
   - **Body**: `{ input: { text }, voice: voiceId || "default", model: modelId }`
   - **Response**: Binary WAV audio converted to `{ base64, format: "wav" }`.
5. **Speech-to-Text (`transcribeNvidia` in `sttCore.js`)**:
   - Sends multipart `FormData` with `file` and `model`, normalizing `{ text: data.text || data.transcript || "" }`.

---

## 3. Supported Models
| Model ID | Display Name | Kind |
|---|---|---|
| `minimaxai/minimax-m2.7` | MiniMax M2.7 | `llm` |
| `minimaxai/minimax-m3` | MiniMax M3 | `llm` |
| `z-ai/glm-5.2` | GLM 5.2 | `llm` |
| `deepseek-ai/deepseek-v4-pro` | DeepSeek V4 Pro | `llm` |
| `deepseek-ai/deepseek-v4-flash` | DeepSeek V4 Flash | `llm` |
| `moonshotai/kimi-k2.6` | Kimi K2.6 | `llm` |
| `nvidia/nemotron-3-ultra-550b-a55b` | Nemotron 3 Ultra | `llm` |
| `nvidia/nv-embedqa-e5-v5` | NV EmbedQA E5 v5 | `embedding` |
| `nvidia/parakeet-ctc-1.1b-asr` | Parakeet CTC 1.1B | `stt` |
| `fastpitch` | FastPitch | `tts` |
| `tacotron2` | Tacotron2 | `tts` |

---

## 4. Verbatim Source Code

### `open-sse/providers/registry/nvidia.js`
```javascript
export default {
  id: "nvidia",
  priority: 20,
  hasFree: true,
  alias: "nvidia",
  display: {
    name: "NVIDIA NIM",
    icon: "developer_board",
    color: "#76B900",
    textIcon: "NV",
    website: "https://developer.nvidia.com/nim",
    notice: {
      text: "Free access for NVIDIA Developer Program members (prototyping & testing).",
      apiKeyUrl: "https://build.nvidia.com/settings/api-keys",
    },
  },
  category: "freeTier",
  authType: "apikey",
  authModes: ["apikey"],
  transport: {
    baseUrl: "https://integrate.api.nvidia.com/v1/chat/completions",
    validateUrl: "https://integrate.api.nvidia.com/v1/models",
  },
  models: [
    { id: "minimaxai/minimax-m2.7", name: "MiniMax M2.7" },
    { id: "minimaxai/minimax-m3", name: "MiniMax M3" },
    { id: "z-ai/glm-5.2", name: "GLM 5.2" },
    { id: "deepseek-ai/deepseek-v4-pro", name: "DeepSeek V4 Pro" },
    { id: "deepseek-ai/deepseek-v4-flash", name: "DeepSeek V4 Flash" },
    { id: "moonshotai/kimi-k2.6", name: "Kimi K2.6" },
    { id: "nvidia/nemotron-3-ultra-550b-a55b", name: "Nemotron 3 Ultra" },
    { id: "nvidia/nv-embedqa-e5-v5", name: "NV EmbedQA E5 v5", kind: "embedding" },
    { id: "nvidia/parakeet-ctc-1.1b-asr", name: "Parakeet CTC 1.1B", params: ["language"], kind: "stt" },
    { id: "fastpitch", name: "FastPitch", kind: "tts" },
    { id: "tacotron2", name: "Tacotron2", kind: "tts" },
  ],
  serviceKinds: ["llm","tts","embedding"],
  ttsConfig: {
    baseUrl: "https://integrate.api.nvidia.com/v1/audio/speech",
    authType: "apikey",
    authHeader: "bearer",
    format: "nvidia-tts",
  },
  embeddingConfig: { baseUrl: "https://integrate.api.nvidia.com/v1/embeddings", authType: "apikey", authHeader: "bearer" },
};
```
