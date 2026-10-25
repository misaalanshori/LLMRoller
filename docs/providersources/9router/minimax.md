# MiniMax Coding (`minimax`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `minimax`
- **Priority**: `90`
- **Alias**: `minimax`
- **Display Name**: Minimax Coding (International `api.minimax.io`)
- **Category**: `apikey`
- **Service Kinds**: `["llm", "image", "imageToText", "webSearch", "tts"]`
- **Website**: `https://www.minimaxi.com`
- **API Key URL**: `https://platform.minimaxi.com/user-center/basic-information/interface-key`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source Files**:
  - `open-sse/providers/registry/minimax.js`
  - `open-sse/handlers/ttsProviders/minimax.js`

---

## 2. Endpoints by Service Kind
1. **Claude / Anthropic Format Transport (Default `transport` & `transports[1]`)**:
   - **Endpoint**: `POST https://api.minimax.io/anthropic/v1/messages?beta=true`
   - **Auth**: `x-api-key: <apiKey>` (`scheme: "raw"`)
   - **Headers**: `CLAUDE_API_HEADERS`
   - **Quirks**: `dropOutputConfig: true`, `requireClaudeToolType: true`, `reasoningInject: { scope: "all" }`
2. **OpenAI Format Transport (`transports[0]`)**:
   - **Endpoint**: `POST https://api.minimax.io/v1/chat/completions`
   - **Auth**: `Authorization: Bearer <apiKey>`
3. **Text-to-Speech (`ttsConfig` -> `minimax.js`)**:
   - **Endpoint**: `POST https://api.minimax.io/v1/t2a_v2`
   - **Auth**: `Authorization: Bearer <apiKey>`
   - **Format**: `"minimax-tts"` (hex-encoded MP3 audio response decoded via `hexToBase64`)
4. **Image Generation (`imageConfig`)**:
   - **Endpoint**: `POST https://api.minimaxi.com/v1/images/generations`
5. **Web Search via Chat (`searchViaChat`)**:
   - **Endpoint**: `POST https://api.minimaxi.com/v1/text/chatcompletion_v2` (`defaultModel: "MiniMax-M2.7"`)
6. **Usage / Quota Endpoints**:
   - `https://www.minimax.io/v1/token_plan/remains`
   - `https://api.minimax.io/v1/api/openplatform/coding_plan/remains`

---

## 3. Supported Models
| Model ID | Display Name | Kind | Extra Metadata |
|---|---|---|---|
| `MiniMax-M3` | MiniMax M3 | `llm` | `targetFormat: "claude"` |
| `MiniMax-M2.7` | MiniMax M2.7 | `llm` | — |
| `MiniMax-M2.5` | MiniMax M2.5 | `llm` | — |
| `MiniMax-M2.1` | MiniMax M2.1 | `llm` | — |
| `minimax-image-01` | MiniMax Image 01 | `image` | `params: ["n", "size", "response_format"]` |
| `speech-2.8-hd`, `speech-2.8-turbo`, `speech-2.6-hd`, `speech-2.6-turbo`, `speech-02-hd`, `speech-02-turbo`, `speech-01-hd`, `speech-01-turbo` | Speech TTS Series | `tts` | — |

---

## 4. Verbatim Source Code Snippets

### `open-sse/providers/registry/minimax.js`
```javascript
import { CLAUDE_API_HEADERS } from "../shared.js";

export default {
  id: "minimax",
  priority: 90,
  alias: "minimax",
  display: {
    name: "Minimax Coding",
    icon: "memory",
    color: "#7C3AED",
    textIcon: "MM",
    website: "https://www.minimaxi.com",
    notice: {
      apiKeyUrl: "https://platform.minimaxi.com/user-center/basic-information/interface-key",
    },
  },
  category: "apikey",
  transport: {
    baseUrl: "https://api.minimax.io/anthropic/v1/messages",
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
        "https://www.minimax.io/v1/token_plan/remains",
        "https://api.minimax.io/v1/api/openplatform/coding_plan/remains",
      ],
    },
  },
  transports: [
    {
      format: "openai",
      baseUrl: "https://api.minimax.io/v1/chat/completions",
      auth: { combined: true, header: "Authorization", scheme: "bearer" },
    },
    {
      format: "claude",
      baseUrl: "https://api.minimax.io/anthropic/v1/messages",
      urlSuffix: "?beta=true",
      headers: { ...CLAUDE_API_HEADERS },
      auth: { combined: true, header: "x-api-key", scheme: "raw" },
    },
  ],
  serviceKinds: ["llm","image","imageToText","webSearch","tts"],
  ttsConfig: { baseUrl: "https://api.minimax.io/v1/t2a_v2", authType: "apikey", authHeader: "bearer", format: "minimax-tts" },
  imageConfig: { baseUrl: "https://api.minimaxi.com/v1/images/generations" },
  searchViaChat: {
    defaultModel: "MiniMax-M2.7",
    endpoint: "https://api.minimaxi.com/v1/text/chatcompletion_v2",
    pricingUrl: "https://www.minimaxi.com/document/price",
  },
};
```
