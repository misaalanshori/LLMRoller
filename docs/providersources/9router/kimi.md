# Kimi (`kimi`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `kimi`
- **Priority**: `170`
- **Primary Alias**: `kimi`
- **Additional Aliases**: `["kimi-coding", "kmc"]`
- **Display Name**: Kimi (Moonshot AI / Kimi Code)
- **Category**: `oauth`
- **Auth Modes**: `["oauth", "apikey"]`
- **Service Kinds**: `["llm", "webSearch"]`
- **Website**: `https://kimi.moonshot.cn` (`https://www.kimi.com/code`)
- **API Key URL**: `https://platform.moonshot.ai/console/api-keys`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source Files**:
  - `open-sse/providers/registry/kimi.js`
  - `open-sse/config/appConstants.js` (`buildKimiHeaders`)
  - `src/lib/oauth/providers/kimi.js`

---

## 2. Tri-Endpoint Multi-Format Transport & Search
Kimi declares three native format endpoints under `transports[]` so `9router` routes directly in the client's native format:
1. **Claude Format (`format: "claude"`, also default `transport`)**:
   - **Endpoint**: `POST https://api.kimi.com/coding/v1/messages?beta=true`
   - **Auth**: `x-api-key: <token>` (`scheme: "raw"`)
   - **Headers**: `CLAUDE_API_HEADERS` + `kimiHeaders`
2. **OpenAI Chat Completions Format (`format: "openai"`)**:
   - **Endpoint**: `POST https://api.kimi.com/coding/v1/chat/completions`
   - **Auth**: `Authorization: Bearer <token>` + `kimiHeaders`
3. **OpenAI Responses API Format (`format: "openai-responses"`)**:
   - **Endpoint**: `POST https://api.kimi.com/coding/v1/responses`
   - **Auth**: `Authorization: Bearer <token>` + `kimiHeaders`
4. **Web Search via Chat (`searchViaChat`)**:
   - **Endpoint**: `POST https://api.moonshot.cn/v1/chat/completions` (`defaultModel: "kimi-k3"`)

---

## 3. Device Telemetry Headers (`kimiHeaders` -> `buildKimiHeaders`) & OAuth Device Flow
- **`buildKimiHeaders(deviceId)` (`open-sse/config/appConstants.js`)**:
  ```javascript
  {
    "X-Msh-Platform": "9router",
    "X-Msh-Version": "<app_package_version>",
    "X-Msh-Device-Name": "<os.hostname()>",
    "X-Msh-Device-Model": "macOS arm64 | Linux x64 | Windows x64",
    "X-Msh-Device-Id": "<deviceId || kimi-timestamp>"
  }
  ```
- **OAuth 2.0 Device Authorization Flow (`src/lib/oauth/providers/kimi.js`)**:
  - `clientId`: `17e5f671-d194-4dfb-9706-5516cb48c098`
  - `deviceCodeUrl`: `POST https://auth.kimi.com/api/oauth/device_authorization` (sends `client_id` + `buildKimiHeaders(deviceId)`)
  - `authorizeDeviceUrl`: `https://www.kimi.com/code/authorize_device`
  - `tokenUrl` / `refreshUrl`: `POST https://auth.kimi.com/api/oauth/token`
  - `refreshLeadMs`: `300000` (5 minutes)

---

## 4. Supported Models
| Model ID | Display Name |
|---|---|
| `kimi-k3` | Kimi K3 |
| `k3` | Kimi K3 (Code) |
| `kimi-for-coding` | Kimi for Coding |
| `kimi-for-coding-highspeed` | Kimi for Coding Highspeed |
| `kimi-k2.7-code` | Kimi K2.7 Code |
| `kimi-k2.7-code-highspeed` | Kimi K2.7 Code Highspeed |
| `kimi-k2.6` | Kimi K2.6 |
| `kimi-k2.5` | Kimi K2.5 |
| `kimi-k2.5-thinking` | Kimi K2.5 Thinking |
| `kimi-latest` | Kimi Latest |

---

## 5. Verbatim Source Code Snippets

### `open-sse/providers/registry/kimi.js` (Excerpt)
```javascript
export default {
  id: "kimi",
  priority: 170,
  alias: "kimi",
  aliases: ["kimi-coding", "kmc"],
  category: "oauth",
  authModes: ["oauth", "apikey"],
  hasOAuth: true,
  transport: {
    baseUrl: "https://api.kimi.com/coding/v1/messages",
    format: "claude",
    urlSuffix: "?beta=true",
    headers: { ...CLAUDE_API_HEADERS },
    clientId: "17e5f671-d194-4dfb-9706-5516cb48c098",
    tokenUrl: "https://auth.kimi.com/api/oauth/token",
    refreshUrl: "https://auth.kimi.com/api/oauth/token",
    auth: {
      combined: true,
      header: "x-api-key",
      scheme: "raw",
      hooks: ["kimiHeaders"],
    },
  },
  transports: [
    {
      format: "openai",
      baseUrl: "https://api.kimi.com/coding/v1/chat/completions",
      auth: { combined: true, header: "Authorization", scheme: "bearer", hooks: ["kimiHeaders"] },
    },
    {
      format: "claude",
      baseUrl: "https://api.kimi.com/coding/v1/messages",
      urlSuffix: "?beta=true",
      headers: { ...CLAUDE_API_HEADERS },
      auth: { combined: true, header: "x-api-key", scheme: "raw", hooks: ["kimiHeaders"] },
    },
    {
      format: "openai-responses",
      baseUrl: "https://api.kimi.com/coding/v1/responses",
      auth: { combined: true, header: "Authorization", scheme: "bearer", hooks: ["kimiHeaders"] },
    },
  ],
};
```
