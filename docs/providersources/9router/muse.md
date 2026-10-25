# Muse (`muse`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `muse`
- **Priority**: `120`
- **Primary Alias**: `muse`
- **Additional Aliases**: `["muse-ai", "meta-model-api", "muse-code", "muse-subscription"]`
- **UI Alias**: `muse`
- **Display Name**: Muse (Meta Model API)
- **Category**: `oauth`
- **Auth Modes**: `["oauth", "apikey"]`
- **Website**: `https://muse.ai` (`https://dev.meta.ai`)
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source Files**:
  - `open-sse/providers/registry/muse.js`
  - `open-sse/executors/default.js` (`museHeaders` hook)
  - `src/lib/oauth/providers/muse.js`

---

## 2. Multi-Endpoint Transport & Meta OIDC Device Code Key Minting
1. **OpenAI Chat Completions Transport (`transports[0]` & default `transport`)**:
   - **Endpoint**: `POST https://api.meta.ai/v1/chat/completions`
   - **Models / Validation Endpoint**: `GET https://api.meta.ai/v1/models`
2. **OpenAI Responses API Transport (`transports[1]`, pinned by all Muse Spark models)**:
   - **Endpoint**: `POST https://api.meta.ai/v1/responses`
   - **Thinking Format**: `"openai-responses"` (requires `reasoning: { effort, summary }`, rejects top-level `reasoning_effort`)
3. **Meta OIDC Device Code + Subscription Key Minting (`src/lib/oauth/providers/muse.js`)**:
   - `clientId`: `"1031625952748946"`
   - **Device Code Request**: `POST https://auth.meta.com/oidc/device/authorization/` (`x-api-version: 1.0.0`, `client_id=1031625952748946`)
   - **Token Poll**: `POST https://auth.meta.com/oidc/device/token/` (`grant_type=urn:ietf:params:oauth:grant-type:device_code`)
   - **Subscription API Key Minting (`postExchange`)**:
     - `POST https://api.meta.ai/muse-code/key`
     - Headers: `Authorization: Bearer <oauth_access_token>`, `x-api-version: 1.0.0`, `Content-Type: application/json`
     - Body: `{"onboard": true}`
     - Returns `{ api_key: "LLM|...", is_subs_active: true, subs_tier_name, user_email }`. The minted `api_key` is stored as `credentials.accessToken`.

---

## 3. Authentication & `museHeaders` Hook
- **Authorization Header**: `Authorization: Bearer <apiKey_or_mintedAccessToken>`
- **Subscription Header Hook (`HEADER_HOOKS.museHeaders`)**:
  ```javascript
  // Muse: x-api-version only on subscription (minted key) requests — plain
  // Model API keys already work without it
  museHeaders: (h, c) => { if (c?.accessToken && !c?.apiKey) h["x-api-version"] = "1.0.0"; }
  ```

---

## 4. Supported Models
| Model ID | Display Name | Target Format |
|---|---|---|
| `muse-spark-1.3` | Muse Spark 1.3 | `openai-responses` |
| `muse-spark-1.2` | Muse Spark 1.2 | `openai-responses` |
| `muse-spark-1.1` | Muse Spark 1.1 | `openai-responses` |
| `muse-spark-1.3-contributor` | Muse Spark 1.3 Contributor | `openai-responses` |
| `muse-spark-1.2-contributor` | Muse Spark 1.2 Contributor | `openai-responses` |

---

## 5. Verbatim Source Code Snippets

### `open-sse/providers/registry/muse.js`
```javascript
export default {
  id: "muse",
  priority: 120,
  alias: "muse",
  aliases: [
    "muse-ai",
    "meta-model-api",
    "muse-code",
    "muse-subscription",
  ],
  uiAlias: "muse",
  category: "oauth",
  authModes: ["oauth", "apikey"],
  hasOAuth: true,
  transport: {
    baseUrl: "https://api.meta.ai/v1/chat/completions",
    validateUrl: "https://api.meta.ai/v1/models",
    modelsUrl: "https://api.meta.ai/v1/models",
    auth: { combined: true, header: "Authorization", scheme: "bearer", hooks: ["museHeaders"] },
  },
  transports: [
    {
      format: "openai",
      baseUrl: "https://api.meta.ai/v1/chat/completions",
      auth: { combined: true, header: "Authorization", scheme: "bearer", hooks: ["museHeaders"] },
    },
    {
      format: "openai-responses",
      baseUrl: "https://api.meta.ai/v1/responses",
      auth: { combined: true, header: "Authorization", scheme: "bearer", hooks: ["museHeaders"] },
      thinkingFormat: "openai-responses",
    },
  ],
  models: [
    { id: "muse-spark-1.3", name: "Muse Spark 1.3", targetFormat: "openai-responses", supportedFormats: ["openai-responses"] },
    { id: "muse-spark-1.2", name: "Muse Spark 1.2", targetFormat: "openai-responses", supportedFormats: ["openai-responses"] },
    { id: "muse-spark-1.1", name: "Muse Spark 1.1", targetFormat: "openai-responses", supportedFormats: ["openai-responses"] },
    { id: "muse-spark-1.3-contributor", name: "Muse Spark 1.3 Contributor", targetFormat: "openai-responses", supportedFormats: ["openai-responses"] },
    { id: "muse-spark-1.2-contributor", name: "Muse Spark 1.2 Contributor", targetFormat: "openai-responses", supportedFormats: ["openai-responses"] },
  ],
  passthroughModels: true,
  oauth: {
    clientId: "1031625952748946",
    deviceCodeUrl: "https://auth.meta.com/oidc/device/authorization/",
    tokenUrl: "https://auth.meta.com/oidc/device/token/",
  },
};
```
