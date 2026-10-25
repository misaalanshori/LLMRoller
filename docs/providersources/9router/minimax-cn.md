# MiniMax China (`minimax-cn`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `minimax-cn`
- **Priority**: `190`
- **Alias**: `minimax-cn`
- **Display Name**: Minimax (China)
- **Category**: `apikey`
- **Service Kinds**: `["llm", "tts"]`
- **Website**: `https://www.minimaxi.com`
- **API Key URL**: `https://platform.minimaxi.com/user-center/basic-information/interface-key`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source Files**:
  - `open-sse/providers/registry/minimax-cn.js`
  - `open-sse/handlers/ttsProviders/minimax.js`

---

## 2. Multi-Endpoint Transport & TTS Configuration
1. **Claude / Anthropic Format Transport (Default `transport` & `transports[1]`)**:
   - **Endpoint**: `POST https://api.minimaxi.com/anthropic/v1/messages?beta=true`
   - **Auth**: `x-api-key: <apiKey>` (`scheme: "raw"`)
   - **Headers**: `CLAUDE_API_HEADERS`
   - **Quirks**: `dropOutputConfig: true`, `requireClaudeToolType: true`, `reasoningInject: { scope: "all" }`
2. **OpenAI Format Transport (`transports[0]`)**:
   - **Endpoint**: `POST https://api.minimaxi.com/v1/chat/completions`
   - **Auth**: `Authorization: Bearer <apiKey>`
3. **Text-to-Speech (`ttsConfig` -> `open-sse/handlers/ttsProviders/minimax.js`)**:
   - **Endpoint**: `POST https://api.minimaxi.com/v1/t2a_v2`
   - **Auth**: `Authorization: Bearer <apiKey>`
   - **Payload**: `{ model, text, stream: false, language_boost: "auto", output_format: "hex", voice_setting: { voice_id: voiceId || "English_expressive_narrator", speed: 1, vol: 1, pitch: 0 }, audio_setting: { sample_rate: 32000, bitrate: 128000, format: "mp3", channel: 1 } }`
   - **Response**: Converts hex-encoded `data.data.audio` to Base64 via `hexToBase64`.
4. **Usage / Quota Endpoints**:
   - `https://www.minimaxi.com/v1/api/openplatform/coding_plan/remains`
   - `https://api.minimaxi.com/v1/api/openplatform/coding_plan/remains`

---

## 3. Supported Models
| Model ID | Display Name | Kind | Extra Metadata |
|---|---|---|---|
| `MiniMax-M3` | MiniMax M3 | `llm` | `targetFormat: "claude"` |
| `MiniMax-M2.7` | MiniMax M2.7 | `llm` | — |
| `MiniMax-M2.5` | MiniMax M2.5 | `llm` | — |
| `MiniMax-M2.1` | MiniMax M2.1 | `llm` | — |
| `speech-2.8-hd`, `speech-2.8-turbo`, `speech-2.6-hd`, `speech-2.6-turbo`, `speech-02-hd`, `speech-02-turbo`, `speech-01-hd`, `speech-01-turbo` | Speech TTS Series | `tts` | — |

---

## 4. Verbatim Source Code

### `open-sse/providers/registry/minimax-cn.js`
```javascript
import { CLAUDE_API_HEADERS } from "../shared.js";

export default {
  id: "minimax-cn",
  priority: 190,
  alias: "minimax-cn",
  display: {
    name: "Minimax (China)",
    icon: "memory",
    color: "#DC2626",
    textIcon: "MC",
    website: "https://www.minimaxi.com",
    notice: {
      apiKeyUrl: "https://platform.minimaxi.com/user-center/basic-information/interface-key",
    },
  },
  category: "apikey",
  transport: {
    baseUrl: "https://api.minimaxi.com/anthropic/v1/messages",
    format: "claude",
    urlSuffix: "?beta=true",
    headers: { ...CLAUDE_API_HEADERS },
    quirks: {
      dropOutputConfig: true,
      requireClaudeToolType: true,
    },
    reasoningInject: {
      scope: "all",
    },
    auth: {
      combined: true,
      header: "x-api-key",
      scheme: "raw",
    },
    usage: {
      urls: [
        "https://www.minimaxi.com/v1/api/openplatform/coding_plan/remains",
        "https://api.minimaxi.com/v1/api/openplatform/coding_plan/remains",
      ],
    },
  },
  transports: [
    {
      format: "openai",
      baseUrl: "https://api.minimaxi.com/v1/chat/completions",
      auth: { combined: true, header: "Authorization", scheme: "bearer" },
    },
    {
      format: "claude",
      baseUrl: "https://api.minimaxi.com/anthropic/v1/messages",
      urlSuffix: "?beta=true",
      headers: { ...CLAUDE_API_HEADERS },
      auth: { combined: true, header: "x-api-key", scheme: "raw" },
    },
  ],
  serviceKinds: ["llm","tts"],
  ttsConfig: { baseUrl: "https://api.minimaxi.com/v1/t2a_v2", authType: "apikey", authHeader: "bearer", format: "minimax-tts" },
};
```
