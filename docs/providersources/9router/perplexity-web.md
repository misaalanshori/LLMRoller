# Perplexity Web (`perplexity-web`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `perplexity-web`
- **Priority**: `220`
- **Primary Alias**: `perplexity-web`
- **Additional Aliases**: `["pw"]`
- **UI Alias**: `pw`
- **Display Name**: Perplexity Web (Pro/Max)
- **Category**: `webCookie`
- **Auth Type**: `cookie` (`__Secure-next-auth.session-token` from `perplexity.ai`)
- **Service Kinds**: `[]` (parked in UI, full LLM SSE implementation in `PerplexityWebExecutor`)
- **Website**: `https://www.perplexity.ai`
- **Executor**: `PerplexityWebExecutor` (`open-sse/executors/perplexity-web.js`)
- **Source Files**:
  - `open-sse/providers/registry/perplexity-web.js`
  - `open-sse/executors/perplexity-web.js`

---

## 2. Endpoint, Headers & Request Payload
- **Endpoint**: `POST https://www.perplexity.ai/rest/sse/perplexity_ask`
- **Headers**:
  - `Content-Type`: `"application/json"`
  - `Accept`: `"text/event-stream"`
  - `Origin`: `"https://www.perplexity.ai"`
  - `Referer`: `"https://www.perplexity.ai/"`
  - `User-Agent`: `"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36"`
  - `X-App-ApiClient`: `"default"`
  - `X-App-ApiVersion`: `"2.18"`
  - `Cookie`: `__Secure-next-auth.session-token=<apiKey>` (or `Authorization: Bearer <accessToken>`)
- **Request Payload (`buildPplxRequestBody`)**:
  ```json
  {
    "query_str": "<query>",
    "params": {
      "query_str": "<query>",
      "search_focus": "internet",
      "mode": "concise | copilot",
      "model_preference": "<modelPref>",
      "sources": ["web"],
      "attachments": [],
      "frontend_uuid": "<uuid>",
      "frontend_context_uuid": "<uuid>",
      "version": "2.18",
      "language": "en-US",
      "timezone": "<IANA_tz>",
      "search_recency_filter": null,
      "is_incognito": true,
      "use_schematized_api": true,
      "last_backend_uuid": "<followUpUuid_or_null>"
    }
  }
  ```
- **Multi-Turn Session Continuation (`sessionKey` / `sessionLookup` / `sessionStore`)**:
  - Hashes conversation history with FNV-1a (`0x811c9dc5`, prime `0x01000193`) to look up `backend_uuid` from prior turns (`SESSION_MAX_AGE_MS = 3600000`), passing it as `last_backend_uuid`.

---

## 3. Supported Models & `MODEL_MAP` / `THINKING_MAP`
| Model ID | Display Name | Default `[mode, model_preference]` | Thinking `model_preference` |
|---|---|---|---|
| `pplx-auto` | Perplexity Auto (Free) | `["concise", "pplx_pro"]` | — |
| `pplx-sonar` | Perplexity Sonar | `["copilot", "experimental"]` | — |
| `pplx-gpt` | GPT-5.4 (via Perplexity) | `["copilot", "gpt54"]` | `"gpt54_thinking"` |
| `pplx-gemini` | Gemini 3.1 Pro (via Perplexity) | `["copilot", "gemini31pro_high"]` | — |
| `pplx-sonnet` | Claude Sonnet 4.6 (via Perplexity) | `["copilot", "claude46sonnet"]` | `"claude46sonnetthinking"` |
| `pplx-opus` | Claude Opus 4.6 (via Perplexity) | `["copilot", "claude46opus"]` | `"claude46opusthinking"` |
| `pplx-nemotron` | Nemotron 3 Super (via Perplexity) | `["copilot", "nv_nemotron_3_super"]` | — |

---

## 4. Verbatim Source Code Snippets

### `open-sse/executors/perplexity-web.js` (Excerpt)
```javascript
const MODEL_MAP = {
  "pplx-auto": ["concise", "pplx_pro"],
  "pplx-sonar": ["copilot", "experimental"],
  "pplx-gpt": ["copilot", "gpt54"],
  "pplx-gemini": ["copilot", "gemini31pro_high"],
  "pplx-sonnet": ["copilot", "claude46sonnet"],
  "pplx-opus": ["copilot", "claude46opus"],
  "pplx-nemotron": ["copilot", "nv_nemotron_3_super"],
};

const THINKING_MAP = {
  "pplx-gpt": "gpt54_thinking",
  "pplx-sonnet": "claude46sonnetthinking",
  "pplx-opus": "claude46opusthinking",
};

function buildPplxRequestBody(query, mode, modelPref, followUpUuid) {
  const tz = typeof Intl !== "undefined" ? Intl.DateTimeFormat().resolvedOptions().timeZone : "UTC";
  return {
    query_str: query,
    params: {
      query_str: query,
      search_focus: "internet",
      mode,
      model_preference: modelPref,
      sources: ["web"],
      attachments: [],
      frontend_uuid: crypto.randomUUID(),
      frontend_context_uuid: crypto.randomUUID(),
      version: PPLX_API_VERSION,
      language: "en-US",
      timezone: tz,
      search_recency_filter: null,
      is_incognito: true,
      use_schematized_api: true,
      last_backend_uuid: followUpUuid,
    },
  };
}
```
