# Qoder CN (`qoder-cn`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `qoder-cn`
- **Priority**: `30`
- **Alias / UI Alias**: `qdcn`
- **Display Name**: Qoder CN
- **Category**: `oauth`
- **Auth Modes**: `["oauth", "apikey"]` (accepts device tokens `dt-...`, job tokens `jt-...`, or Personal Access Tokens `pt-...` from `https://qoder.com.cn/account/integrations`)
- **Website**: `https://qoder.com.cn`
- **Executor**: `QoderExecutor` (`new QoderExecutor("qoder-cn")`, `this.region = "cn"`)
- **Source Files**:
  - `open-sse/providers/registry/qoder-cn.js`
  - `open-sse/executors/qoder.js`
  - `open-sse/shared/qoder/cosy.js`
  - `open-sse/shared/qoder/encoding.js`
  - `src/lib/oauth/providers/qoder-cn.js`

---

## 2. China Regional Endpoints & OAuth / PAT Flow
- **Inference Endpoint**:
  - `POST https://gateway.qoder.com.cn/algo/api/v2/service/pro/sse/agent_chat_generation?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1`
- **Model Catalog Endpoint**:
  - `GET https://gateway.qoder.com.cn/algo/api/v2/model/list`
- **Queue Status Poll Endpoint**:
  - `GET https://gateway.qoder.com.cn/algo/api/v2/service/pro/queue/status`
- **OAuth & Quota Endpoints (`oauth` block)**:
  - `openApiBaseUrl`: `https://openapi.qoder.com.cn`
  - `centerBaseUrl` / `chatBaseUrl`: `https://gateway.qoder.com.cn`
  - `deviceTokenUrl`: `https://openapi.qoder.com.cn/api/v1/deviceToken/poll`
  - `refreshUrl`: `https://gateway.qoder.com.cn/algo/api/v3/user/refresh_token`
  - `userInfoUrl`: `https://openapi.qoder.com.cn/api/v1/userinfo`
  - `quotaUsageUrl`: `https://openapi.qoder.com.cn/api/v2/quota/usage`
  - `loginUrl`: `https://qoder.com.cn/device/selectAccounts`

---

## 3. WAF-Bypass Body Encoding (`qoderEncodeBody`) & COSY Hybrid RSA+AES+MD5 Signing
Shares the exact same `QoderExecutor`, `qoderEncodeBody`, and `buildCosyHeaders` implementation as `qoder` (see `qoder.md` for the full cryptographic and request payload specification), pointed at the `gateway.qoder.com.cn` / `openapi.qoder.com.cn` hosts (`this.region = "cn"`).

---

## 4. Supported Models
| Model ID | Display Name |
|---|---|
| `ultimate` | Ultimate |
| `auto` | Auto |
| `performance` | Performance |
| `efficient` | Efficient |
| `lite` | Lite |
| `qmodel_38max` | Qwen3.8-Max |
| `qmodel_latest` | Qwen3.7-Max |
| `qmodel` | Qwen3.7-Plus |
| `qfmodel` | Qwen3.8-Flash |
| `kmodel_latest` | Kimi-K3 |
| `kmodel` | Kimi-K2.7-Code |
| `gmodel` | GLM-5.3 |
| `gfmodel` | GLM-5.3-Flash |
| `dmodel` | DeepSeek-V4-Pro |
| `dfmodel` | DeepSeek-V4-Flash |
| `mmodel` | MiniMax-M3 |

---

## 5. Verbatim Source Code

### `open-sse/providers/registry/qoder-cn.js`
```javascript
export default {
  id: "qoder-cn",
  priority: 30,
  alias: "qdcn",
  uiAlias: "qdcn",
  display: {
    name: "Qoder CN",
    icon: "water_drop",
    color: "#EC4899",
    website: "https://qoder.com.cn",
    notice: {
      signupUrl: "https://qoder.com.cn",
    },
  },
  category: "oauth",
  authModes: ["oauth", "apikey"],
  hasOAuth: true,
  authHint: "Personal Access Token (pt-...) from https://qoder.com.cn/account/integrations",
  transport: {
    baseUrl: "https://gateway.qoder.com.cn/algo/api/v2/service/pro/sse/agent_chat_generation",
    headers: {},
    timeoutMs: 120000,
    stallTimeoutMs: 120000,
    usage: {
      url: "https://openapi.qoder.com.cn/api/v2/quota/usage",
    },
  },
  oauth: {
    openApiBaseUrl: "https://openapi.qoder.com.cn",
    centerBaseUrl: "https://gateway.qoder.com.cn",
    chatBaseUrl: "https://gateway.qoder.com.cn",
    deviceTokenUrl: "https://openapi.qoder.com.cn/api/v1/deviceToken/poll",
    refreshUrl: "https://gateway.qoder.com.cn/algo/api/v3/user/refresh_token",
    userInfoUrl: "https://openapi.qoder.com.cn/api/v1/userinfo",
    quotaUsageUrl: "https://openapi.qoder.com.cn/api/v2/quota/usage",
    loginUrl: "https://qoder.com.cn/device/selectAccounts",
  },
};
```
