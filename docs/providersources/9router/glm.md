# Zai GLM Coding (`glm`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `glm`
- **Priority**: `140`
- **Alias**: `glm`
- **Display Name**: Zai GLM Coding
- **Category**: `oauth`
- **Auth Modes**: `["oauth", "apikey"]`
- **Service Kinds**: `["llm", "webSearch"]`
- **Website**: `https://open.bigmodel.cn` (`https://chat.z.ai`)
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source Files**:
  - `open-sse/providers/registry/glm.js`
  - `src/lib/oauth/providers/glm.js`
  - `open-sse/handlers/search/callers.js` (`buildGlmSearchRequest`)
  - `open-sse/handlers/search/normalizers.js` (`normalizeGlmSearch`)

---

## 2. Multi-Endpoint Transport & MCP Web Search
1. **Claude / Anthropic Format Transport (Default `transport` & `transports[1]`)**:
   - **Endpoint**: `POST https://api.z.ai/api/anthropic/v1/messages?beta=true`
   - **Auth**: `x-api-key: <planApiKey>` (`scheme: "raw"`)
   - **Headers**: `CLAUDE_API_HEADERS` (`Anthropic-Version: 2023-06-01`, `Anthropic-Beta: claude-code-20250219,interleaved-thinking-2025-05-14`)
2. **OpenAI Format Transport (`transports[0]`)**:
   - **Endpoint**: `POST https://api.z.ai/api/coding/paas/v4/chat/completions`
   - **Auth**: `Authorization: Bearer <planApiKey>`
3. **Coding Plan MCP Web Search (`searchConfig` / `buildGlmSearchRequest`)**:
   - **Endpoint**: `POST https://api.z.ai/api/mcp/web_search_prime/mcp`
   - **Auth**: `Authorization: Bearer <planApiKey>`
   - **JSON-RPC 2.0 Request Body**:
     ```json
     {
       "jsonrpc": "2.0",
       "id": "9r-<timestamp>",
       "method": "tools/call",
       "params": {
         "name": "web_search_prime",
         "arguments": {
           "search_query": "<query>",
           "count": 5
         }
       }
     }
     ```
   - **Response Normalization (`normalizeGlmSearch`)**: Unwraps `data.result.content[0].text` (JSON string) and maps `payload.results` (or `payload.news`) to `SearchResult`.
4. **Quota / Usage Endpoint**:
   - `GET https://api.z.ai/api/monitor/usage/quota/limit`

---

## 3. ZCode CLI OAuth Polling & Coding-Plan API Key Minting (`src/lib/oauth/providers/glm.js`)
1. **CLI Flow Init**:
   - Generates random 32-byte hex `pollToken`.
   - `POST https://zcode.z.ai/api/v1/oauth/cli/init` with `Authorization: Bearer <pollToken>` and body `{"provider": "zai"}` -> returns `{ flow_id, authorize_url, poll_interval_sec, expires_at }`.
2. **CLI Flow Poll**:
   - `GET https://zcode.z.ai/api/v1/oauth/cli/poll/<flow_id>` with `Authorization: Bearer <pollToken>` until `data.status === "ready"`, extracting `zaiAccessToken` (`data.zai.access_token`).
3. **Business Token Exchange (`exchangeBusinessToken`)**:
   - `POST https://api.z.ai/api/auth/z/login` with `{"token": "<zaiAccessToken>"}` -> returns `{ data: { access_token: "<businessToken>" } }`.
4. **Customer Info & API Key Creation/Copy (`resolveCodingPlanApiKey`)**:
   - `GET https://api.z.ai/api/biz/customer/getCustomerInfo` -> selects default personal organization (`默认机构`/`default`) and project (`默认项目`/`default`, `projectType !== "2"`).
   - Lists or creates key named `"zcode-api-key"` at `https://api.z.ai/api/biz/v1/organization/<orgId>/projects/<projectId>/api_keys`.
   - Fetches secret at `GET .../api_keys/copy/<apiKey>` and formats the final long-lived credential as `"${apiKey}.${secretKey}"`.

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
| `glm-4.7` | GLM 4.7 |
| `glm-4.6v` | GLM 4.6V (Vision) |

---

## 5. Verbatim Source Code Snippets

### `open-sse/providers/registry/glm.js` (Excerpt)
```javascript
export default {
  id: "glm",
  priority: 140,
  alias: "glm",
  category: "oauth",
  authModes: ["oauth", "apikey"],
  hasOAuth: true,
  oauth: {
    providerId: "zai",
    cliInitUrl: "https://zcode.z.ai/api/v1/oauth/cli/init",
    cliPollUrl: "https://zcode.z.ai/api/v1/oauth/cli/poll",
    businessLoginUrl: "https://api.z.ai/api/auth/z/login",
    apiBaseUrl: "https://api.z.ai",
    planApiKeyName: "zcode-api-key",
  },
  transports: [
    {
      format: "openai",
      baseUrl: "https://api.z.ai/api/coding/paas/v4/chat/completions",
      auth: { combined: true, header: "Authorization", scheme: "bearer" },
    },
    {
      format: "claude",
      baseUrl: "https://api.z.ai/api/anthropic/v1/messages",
      urlSuffix: "?beta=true",
      headers: { ...CLAUDE_API_HEADERS },
      auth: { combined: true, header: "x-api-key", scheme: "raw" },
    },
  ],
  serviceKinds: ["llm", "webSearch"],
  searchConfig: {
    baseUrl: "https://api.z.ai/api/mcp/web_search_prime/mcp",
    method: "POST",
    authType: "apikey",
    authHeader: "bearer",
    costPerQuery: 0,
    searchTypes: ["web"],
    defaultMaxResults: 5,
    maxMaxResults: 50,
    timeoutMs: 10000,
    cacheTTLMs: 300000,
  },
};
```
