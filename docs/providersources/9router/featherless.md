# Featherless (`featherless`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `featherless`
- **Priority**: `65`
- **Primary Alias**: `featherless`
- **Additional Aliases**: `["fl"]`
- **UI Alias**: `fl`
- **Display Name**: Featherless
- **Category**: `apikey`
- **Auth Type**: `apikey`
- **Website**: `https://featherless.ai`
- **API Key URL**: `https://featherless.ai/account/api-keys`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source File**: `open-sse/providers/registry/featherless.js`

---

## 2. Endpoints & Transport
- **Chat Completions Endpoint**: `POST https://api.featherless.ai/v1/chat/completions`
- **Validation / Models Endpoint**: `GET https://api.featherless.ai/v1/models`
- **Wire Format**: `"openai"`

---

## 3. Authentication & Headers
- **Authorization Header**: `Authorization: Bearer <apiKey>`
- **Content-Type**: `application/json`

---

## 4. Supported Models
| Model ID | Display Name |
|---|---|
| `deepseek-ai/DeepSeek-V4-Pro` | DeepSeek V4 Pro |
| `deepseek-ai/DeepSeek-V4-Flash` | DeepSeek V4 Flash |
| `zai-org/GLM-5.2` | GLM 5.2 |
| `zai-org/GLM-5.1` | GLM 5.1 |
| `moonshotai/Kimi-K2.7-Code` | Kimi K2.7 Code |
| `moonshotai/Kimi-K2.6` | Kimi K2.6 |
| `moonshotai/Kimi-K2.5` | Kimi K2.5 |

---

## 5. Verbatim Source Code

### `open-sse/providers/registry/featherless.js`
```javascript
export default {
  id: "featherless",
  priority: 65,
  alias: "featherless",
  aliases: [
    "fl",
  ],
  uiAlias: "fl",
  display: {
    name: "Featherless",
    icon: "flutter_dash",
    color: "#111827",
    textIcon: "FL",
    website: "https://featherless.ai",
    notice: {
      apiKeyUrl: "https://featherless.ai/account/api-keys",
    },
  },
  category: "apikey",
  authType: "apikey",
  transport: {
    baseUrl: "https://api.featherless.ai/v1/chat/completions",
    validateUrl: "https://api.featherless.ai/v1/models",
  },
  models: [
    { id: "deepseek-ai/DeepSeek-V4-Pro", name: "DeepSeek V4 Pro" },
    { id: "deepseek-ai/DeepSeek-V4-Flash", name: "DeepSeek V4 Flash" },
    { id: "zai-org/GLM-5.2", name: "GLM 5.2" },
    { id: "zai-org/GLM-5.1", name: "GLM 5.1" },
    { id: "moonshotai/Kimi-K2.7-Code", name: "Kimi K2.7 Code" },
    { id: "moonshotai/Kimi-K2.6", name: "Kimi K2.6" },
    { id: "moonshotai/Kimi-K2.5", name: "Kimi K2.5" },
  ],
};
```
