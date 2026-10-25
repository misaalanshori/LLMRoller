# iFlow AI (`iflow`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `iflow`
- **Priority**: `110`
- **Alias**: `if`
- **Hidden**: `true`
- **Display Name**: iFlow AI
- **Category**: `oauth`
- **Website**: `https://iflow.cn`
- **Executor**: `IFlowExecutor` (`open-sse/executors/iflow.js`)
- **Source Files**:
  - `open-sse/providers/registry/iflow.js`
  - `open-sse/executors/iflow.js`
  - `src/lib/oauth/providers/iflow.js`

---

## 2. Endpoints & Two-Stage OAuth + API Key Retrieval Flow
- **Chat Completions Endpoint**: `POST https://apis.iflow.cn/v1/chat/completions` (`thinkingFormat: "openai"`)
- **OAuth 2.0 Authorization Code Flow (`src/lib/oauth/providers/iflow.js`)**:
  - `clientId`: `10009311001`
  - `clientSecret`: `4Z3YjXycVsQvyGF1etiNlIBB4RsqSDtW`
  - `authorizeUrl`: `https://iflow.cn/oauth?loginMethod=phone&type=phone&redirect=<redirectUri>&state=<state>&client_id=10009311001`
  - `tokenUrl`: `POST https://iflow.cn/oauth/token` (uses HTTP Basic Auth `Authorization: Basic base64(clientId:clientSecret)` + form-urlencoded body)
  - `userInfoUrl`: `GET https://iflow.cn/api/oauth/getUserInfo?accessToken=<access_token>`
    - Returns `{ success: true, data: { apiKey, email, phone, nickname } }`. The extracted `data.apiKey` is stored as `credentials.apiKey` and used for both Bearer authentication and HMAC-SHA256 request signing.

---

## 3. HMAC-SHA256 Request Signature & Headers (`IFlowExecutor`)
- **HMAC-SHA256 Signature (`createIFlowSignature`)**:
  - Generates `sessionID = "session-" + crypto.randomUUID()` and `timestamp = Date.now()`.
  - Signs `${userAgent}:${sessionID}:${timestamp}` (`"iFlow-Cli:session-<uuid>:<timestamp>"`) using HMAC-SHA256 with `apiKey` as the secret key, hex-encoded.
- **Request Headers (`buildHeaders`)**:
  - `Content-Type`: `"application/json"`
  - `User-Agent`: `"iFlow-Cli"`
  - `session-id`: `session-<uuid>`
  - `x-iflow-timestamp`: `<timestamp_ms_string>`
  - `x-iflow-signature`: `<hmac_sha256_hex>`
  - `Authorization`: `Bearer <apiKey>`
  - `Accept`: `"text/event-stream"` (when streaming)
- **Request Body Transformation (`transformRequest`)**:
  - Injects `body.stream_options = { include_usage: true }` on streaming requests when `body.messages` is present.

---

## 4. Supported Models
| Model ID | Display Name |
|---|---|
| `qwen3-coder-plus` | Qwen3 Coder Plus |
| `qwen3-max` | Qwen3 Max |
| `qwen3-vl-plus` | Qwen3 VL Plus |
| `qwen3-max-preview` | Qwen3 Max Preview |
| `qwen3-235b` | Qwen3 235B A22B |
| `qwen3-235b-a22b-instruct` | Qwen3 235B A22B Instruct |
| `qwen3-235b-a22b-thinking-2507` | Qwen3 235B A22B Thinking |
| `qwen3-32b` | Qwen3 32B |
| `kimi-k2` | Kimi K2 |
| `deepseek-v3.2` | DeepSeek V3.2 Exp |
| `deepseek-v3.1` | DeepSeek V3.1 Terminus |
| `deepseek-v3` | DeepSeek V3 671B |
| `deepseek-r1` | DeepSeek R1 |
| `glm-4.7` | GLM 4.7 |
| `iflow-rome-30ba3b` | iFlow ROME |

---

## 5. Verbatim Source Code Snippets

### `open-sse/executors/iflow.js`
```javascript
import crypto from "crypto";
import { BaseExecutor } from "./base.js";
import { PROVIDERS } from "../config/providers.js";

export class IFlowExecutor extends BaseExecutor {
  constructor() {
    super("iflow", PROVIDERS.iflow);
  }

  generateUUID() {
    return crypto.randomUUID();
  }

  createIFlowSignature(userAgent, sessionID, timestamp, apiKey) {
    if (!apiKey) return "";
    const payload = `${userAgent}:${sessionID}:${timestamp}`;
    const hmac = crypto.createHmac("sha256", apiKey);
    hmac.update(payload);
    return hmac.digest("hex");
  }

  buildHeaders(credentials, stream = true) {
    const sessionID = `session-${this.generateUUID()}`;
    const timestamp = Date.now();
    const userAgent = this.config.headers["User-Agent"] || "iFlow-Cli";
    const apiKey = credentials.apiKey || credentials.accessToken || "";
    const signature = this.createIFlowSignature(userAgent, sessionID, timestamp, apiKey);

    const headers = {
      "Content-Type": "application/json",
      ...this.config.headers,
      "session-id": sessionID,
      "x-iflow-timestamp": timestamp.toString(),
      "x-iflow-signature": signature
    };

    if (credentials.apiKey) {
      headers["Authorization"] = `Bearer ${credentials.apiKey}`;
    }

    if (stream) {
      headers["Accept"] = "text/event-stream";
    }

    return headers;
  }

  transformRequest(model, body, stream, credentials) {
    if (stream && body.messages && !body.stream_options) {
      body.stream_options = { include_usage: true };
    }
    return body;
  }
}

export default IFlowExecutor;
```
