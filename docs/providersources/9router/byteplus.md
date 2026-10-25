# BytePlus ModelArk (`byteplus`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `byteplus`
- **Priority**: `70`
- **Primary Alias**: `byteplus`
- **Additional Aliases**: `["bpm"]`
- **UI Alias**: `bpm`
- **Display Name**: BytePlus ModelArk
- **Category**: `freeTier`
- **Service Kinds**: `["llm"]`
- **Website**: `https://console.byteplus.com/ark`
- **API Key URL**: `https://console.byteplus.com/ark/region:ark+ap-southeast-1/apiKey`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source File**: `open-sse/providers/registry/byteplus.js`

---

## 2. Endpoints & Transport
- **Chat Completions Endpoint**: `POST https://ark.ap-southeast.bytepluses.com/api/coding/v3/chat/completions`
- **Wire Format**: `"openai"` (default via `DefaultExecutor` / registry defaults)
- **Custom Static Headers**: `{}`

---

## 3. Authentication & Headers
- **Auth Scheme**: Standard Bearer token (`Authorization: Bearer <apiKey>`) via `DefaultExecutor`'s default `BEARER` descriptor.
- **Content-Type**: `application/json`
- **Accept**: `text/event-stream` when `stream: true`, `application/json` when `stream: false`.

---

## 4. Supported Models
| Model ID | Display Name |
|---|---|
| `seed-2-0-pro-260328` | Seed 2.0 Pro |
| `seed-2-0-code-preview-260328` | Seed 2.0 Code Preview |
| `seed-2-0-mini-260215` | Seed 2.0 Mini |
| `seed-2-0-lite-260228` | Seed 2.0 Lite |
| `kimi-k2-thinking-251104` | Kimi K2 Thinking |
| `glm-4-7-251222` | GLM 4.7 |
| `gpt-oss-120b-250805` | GPT-OSS-120B |

---

## 5. Verbatim Source Code

### `open-sse/providers/registry/byteplus.js`
```javascript
export default {
  id: "byteplus",
  priority: 70,
  alias: "byteplus",
  aliases: [
    "bpm",
  ],
  uiAlias: "bpm",
  display: {
    name: "BytePlus ModelArk",
    icon: "cloud",
    color: "#2563EB",
    textIcon: "BP",
    website: "https://console.byteplus.com/ark",
    notice: {
      text: "Free credits for new accounts. Access to Seed 2.0, Kimi K2 Thinking, GLM 4.7, GPT-OSS-120B models.",
      apiKeyUrl: "https://console.byteplus.com/ark/region:ark+ap-southeast-1/apiKey",
    },
  },
  category: "freeTier",
  transport: {
    baseUrl: "https://ark.ap-southeast.bytepluses.com/api/coding/v3/chat/completions",
    headers: {},
  },
  models: [
    { id: "seed-2-0-pro-260328", name: "Seed 2.0 Pro" },
    { id: "seed-2-0-code-preview-260328", name: "Seed 2.0 Code Preview" },
    { id: "seed-2-0-mini-260215", name: "Seed 2.0 Mini" },
    { id: "seed-2-0-lite-260228", name: "Seed 2.0 Lite" },
    { id: "kimi-k2-thinking-251104", name: "Kimi K2 Thinking" },
    { id: "glm-4-7-251222", name: "GLM 4.7" },
    { id: "gpt-oss-120b-250805", name: "GPT-OSS-120B" },
  ],
  serviceKinds: ["llm"],
};
```
