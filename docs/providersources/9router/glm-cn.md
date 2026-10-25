# GLM China (`glm-cn`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `glm-cn`
- **Priority**: `130`
- **Alias**: `glm-cn`
- **Display Name**: GLM (China)
- **Category**: `apikey`
- **Website**: `https://open.bigmodel.cn`
- **API Key URL**: `https://open.bigmodel.cn/usercenter/apikeys`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source File**: `open-sse/providers/registry/glm-cn.js`

---

## 2. Endpoints & Transport
- **Chat Completions Endpoint**: `POST https://open.bigmodel.cn/api/coding/paas/v4/chat/completions`
- **Quota / Usage Endpoint**: `GET https://open.bigmodel.cn/api/monitor/usage/quota/limit`
- **Wire Format**: `"openai"`

---

## 3. Authentication & Headers
- **Authorization Header**: `Authorization: Bearer <apiKey>`
- **Content-Type**: `application/json`

---

## 4. Supported Models
| Model ID | Display Name |
|---|---|
| `glm-5.3` | GLM 5.3 |
| `glm-5.3-flash` | GLM 5.3 Flash (Vision) |
| `glm-5.2` | GLM 5.2 |
| `glm-5.1` | GLM 5.1 |
| `glm-5-turbo` | GLM 5 Turbo |
| `glm-5` | GLM 5 |
| `glm-4.7` | GLM-4.7 |
| `glm-4.6v` | GLM 4.6V (Vision) |
| `glm-4.6` | GLM-4.6 |
| `glm-4.5-air` | GLM-4.5-Air |

---

## 5. Verbatim Source Code

### `open-sse/providers/registry/glm-cn.js`
```javascript
export default {
  id: "glm-cn",
  priority: 130,
  alias: "glm-cn",
  display: {
    name: "GLM (China)",
    icon: "code",
    color: "#DC2626",
    textIcon: "GC",
    website: "https://open.bigmodel.cn",
    notice: {
      apiKeyUrl: "https://open.bigmodel.cn/usercenter/apikeys",
    },
  },
  category: "apikey",
  transport: {
    baseUrl: "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions",
    headers: {},
    usage: {
      url: "https://open.bigmodel.cn/api/monitor/usage/quota/limit",
    },
  },
  models: [
    { id: "glm-5.3", name: "GLM 5.3" },
    { id: "glm-5.3-flash", name: "GLM 5.3 Flash (Vision)" },
    { id: "glm-5.2", name: "GLM 5.2" },
    { id: "glm-5.1", name: "GLM 5.1" },
    { id: "glm-5-turbo", name: "GLM 5 Turbo" },
    { id: "glm-5", name: "GLM 5" },
    { id: "glm-4.7", name: "GLM-4.7" },
    { id: "glm-4.6v", name: "GLM 4.6V (Vision)" },
    { id: "glm-4.6", name: "GLM-4.6" },
    { id: "glm-4.5-air", name: "GLM-4.5-Air" },
  ],
  features: {
    usage: true,
    usageApikey: true,
  },
};
```
