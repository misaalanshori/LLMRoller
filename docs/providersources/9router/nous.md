# Nous Research (`nous`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `nous`
- **Priority**: `65` (`hasFree: true`)
- **Primary Alias**: `nous`
- **Additional Aliases**: `["nous-portal", "nous-research", "ns"]`
- **UI Alias**: `nous`
- **Display Name**: Nous Research
- **Category**: `freeTier`
- **Auth Modes**: `["oauth", "apikey"]`
- **Service Kinds**: `["llm", "imageToText"]`
- **Website**: `https://nousresearch.com` (`https://portal.nousresearch.com`)
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source Files**:
  - `open-sse/providers/registry/nous.js`
  - `open-sse/services/nous.js`
  - `open-sse/executors/default.js` (Nous attribution headers & agent-key refresh)
  - `src/lib/oauth/providers/nous.js`

---

## 2. Endpoints & OAuth Device Code + Agent Key Minting
- **Chat Completions Endpoint**: `POST https://inference-api.nousresearch.com/v1/chat/completions`
- **Models Fetcher Endpoint**: `GET https://inference-api.nousresearch.com/v1/models` (`type: "nous-free"`, `passthroughModels: true`)
- **OAuth 2.0 Device Flow & Agent Key Minting (`src/lib/oauth/providers/nous.js`)**:
  - `clientId`: `"hermes-cli"`
  - `scope`: `"inference:invoke inference:mint_agent_key"`
  - `deviceCodeUrl`: `POST https://portal.nousresearch.com/api/oauth/device/code`
  - `tokenUrl`: `POST https://portal.nousresearch.com/api/oauth/token`
  - `agentKeyUrl` (`postExchange` & `DefaultExecutor.refreshNousTokens`):
    - `POST https://portal.nousresearch.com/api/oauth/agent-key`
    - Headers: `Authorization: Bearer <oauth_access_token>`, `Content-Type: application/json`
    - Body: `{"min_ttl_seconds": 7200}`
    - Returns `{ api_key, key_id, expires_at, expires_in }`. Stored in `credentials.apiKey`.

---

## 3. Headers & OpenRouter-Style Attribution (`DefaultExecutor.buildHeaders`)
- **Authorization Header**: `Authorization: Bearer <agentKey_or_apiKey>`
- **Hermes CLI Attribution Headers (`NOUS_ATTRIBUTION` in `DefaultExecutor.buildHeaders`)**:
  - `HTTP-Referer`: `"https://github.com/NousResearch/hermes-agent"`
  - `X-OpenRouter-Title`: `"Hermes Agent"`
  - `X-OpenRouter-Categories`: `"cli-agent"`
  - `User-Agent`: `"hermes-cli/1.0"`

---

## 4. Seed Models
| Model ID | Display Name | Context Length | Max Output Tokens |
|---|---|---|---|
| `meituan/longcat-2.0:free` | Meituan: LongCat 2.0 | 1,048,756 | 262,144 |
| `meituan/longcat-2.5-preview:free` | Meituan: LongCat 2.5 Preview | 1,048,576 | 131,072 |
| `stepfun/step-5-preview:free` | StepFun: Step 5 Preview | 1,000,000 | 64,000 |
| `upstage/solar-mini4:free` | Upstage: Solar Mini 4 | 524,288 | 131,072 |
| `stepfun/step-3.7-flash:free` | StepFun: Step 3.7 Flash | 262,144 | 256,000 |
| `poolside/laguna-s-2.1:free` | Poolside: Laguna S 2.1 | 262,144 | 131,072 |
| `poolside/laguna-xs-2.1:free` | Poolside: Laguna XS 2.1 | 262,144 | 32,768 |
| `stealth/missingno` | MissingNo | 262,144 | 131,072 |

---

## 5. Verbatim Source Code Snippets

### `open-sse/services/nous.js` (Excerpt)
```javascript
export const NOUS_INFERENCE_BASE_URL = "https://inference-api.nousresearch.com/v1";
export const NOUS_CHAT_COMPLETIONS_URL = `${NOUS_INFERENCE_BASE_URL}/chat/completions`;
export const NOUS_MODELS_URL = `${NOUS_INFERENCE_BASE_URL}/models`;
export const NOUS_PORTAL_BASE_URL = "https://portal.nousresearch.com";
export const NOUS_DEVICE_CODE_URL = `${NOUS_PORTAL_BASE_URL}/api/oauth/device/code`;
export const NOUS_TOKEN_URL = `${NOUS_PORTAL_BASE_URL}/api/oauth/token`;
export const NOUS_AGENT_KEY_URL = `${NOUS_PORTAL_BASE_URL}/api/oauth/agent-key`;
export const NOUS_OAUTH_CLIENT_ID = "hermes-cli";
export const NOUS_OAUTH_SCOPE = "inference:invoke inference:mint_agent_key";
```
