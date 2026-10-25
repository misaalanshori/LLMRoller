# Gemini CLI (`gemini-cli`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `gemini-cli`
- **Priority**: `20` (`hasFree: true`)
- **Alias / UI Alias**: `gc`
- **Display Name**: Gemini CLI
- **Category**: `free`
- **Deprecated**: `true` (`deprecationNotice: "RISK_NOTICE"`)
- **Website**: `https://github.com/google-gemini/gemini-cli`
- **Executor**: `GeminiCLIExecutor` (`open-sse/executors/gemini-cli.js`)
- **Source Files**:
  - `open-sse/providers/registry/gemini-cli.js`
  - `open-sse/executors/gemini-cli.js`
  - `src/lib/oauth/providers/gemini-cli.js`

---

## 2. Endpoints & OAuth 2.0 Flow
- **Base URL**: `https://cloudcode-pa.googleapis.com/v1internal`
- **Streaming Endpoint**: `POST https://cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse`
- **Non-Streaming Endpoint**: `POST https://cloudcode-pa.googleapis.com/v1internal:generateContent`
- **Project Discovery & Usage Endpoints**:
  - `loadCodeAssistUrl`: `POST https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist` (body: `{ metadata: getOAuthClientMetadata(), mode: 1 }`, extracts `data.cloudaicompanionProject.id || data.cloudaicompanionProject`)
  - `quotaUrl`: `https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota`
- **OAuth 2.0 Configuration**:
  - `clientId`: `681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com`
  - `clientSecret`: `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`
  - `authorizeUrl`: `https://accounts.google.com/o/oauth2/v2/auth` (`access_type=offline`, `prompt=consent`)
  - `tokenUrl`: `https://oauth2.googleapis.com/token`
  - `userInfoUrl`: `https://www.googleapis.com/oauth2/v1/userinfo`
  - `scopes`: `["https://www.googleapis.com/auth/cloud-platform", "https://www.googleapis.com/auth/userinfo.email", "https://www.googleapis.com/auth/userinfo.profile"]`

---

## 3. Headers & Request Envelope (`GeminiCLIExecutor`)
- **Headers**:
  - `Content-Type`: `application/json`
  - `Authorization`: `Bearer <accessToken>`
  - `User-Agent`: `geminiCLIUserAgent(model)` (`GeminiCLI/0.34.0/${model} (${os}; ${arch})`)
  - `X-Goog-Api-Client`: `google-genai-sdk/1.41.0 gl-node/v22.19.0`
  - `Accept`: `text/event-stream` (streaming) or `application/json` (non-streaming)
- **Cloud Code Assist Request Envelope (`transformRequest`)**:
  ```json
  {
    "project": "<credentials.projectId>",
    "model": "<model>",
    "request": {
      "contents": [...],
      "systemInstruction": {...},
      "generationConfig": {...},
      "tools": [...]
    }
  }
  ```
- **Error Parsing (`parseError`)**:
  - Parses `error.details[]` on HTTP 429 responses for `@type === "type.googleapis.com/google.rpc.RetryInfo"` and extracts `retryDelay` into `base.retryAfter`.

---

## 4. Supported Models
| Model ID | Display Name |
|---|---|
| `gemini-3.1-pro-preview` | Gemini 3.1 Pro Preview |
| `gemini-3-pro-preview` | Gemini 3 Pro Preview |
| `gemini-3-flash-preview` | Gemini 3 Flash Preview |
| `gemini-3.1-flash-lite-preview` | Gemini 3.1 Flash Lite Preview |
| `gemini-2.5-pro` | Gemini 2.5 Pro |
| `gemini-2.5-flash` | Gemini 2.5 Flash |
| `gemini-2.5-flash-lite` | Gemini 2.5 Flash Lite |

---

## 5. Verbatim Source Code Snippets

### `open-sse/executors/gemini-cli.js`
```javascript
import { BaseExecutor } from "./base.js";
import { PROVIDERS } from "../config/providers.js";
import { OAUTH_ENDPOINTS, GEMINI_CLI_API_CLIENT, geminiCLIUserAgent } from "../config/appConstants.js";

export class GeminiCLIExecutor extends BaseExecutor {
  constructor() {
    super("gemini-cli", PROVIDERS["gemini-cli"]);
  }

  buildUrl(model, stream, urlIndex = 0) {
    const action = stream ? "streamGenerateContent?alt=sse" : "generateContent";
    return `${this.config.baseUrl}:${action}`;
  }

  buildHeaders(credentials, stream = true) {
    return {
      "Content-Type": "application/json",
      "Authorization": `Bearer ${credentials.accessToken}`,
      "User-Agent": geminiCLIUserAgent(this._currentModel),
      "X-Goog-Api-Client": GEMINI_CLI_API_CLIENT,
      "Accept": stream ? "text/event-stream" : "application/json"
    };
  }

  transformRequest(model, body, stream, credentials) {
    this._currentModel = model;
    if (body && body.request && body.model) return body;
    return {
      project: credentials?.projectId || body?.project,
      model,
      request: body
    };
  }
```
