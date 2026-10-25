# OpenAI Codex (`codex`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `codex`
- **Priority**: `30`
- **Alias / UI Alias**: `cx`
- **Display Name**: OpenAI Codex
- **Category**: `oauth`
- **Deprecated**: `true` (`deprecationNotice: "RISK_NOTICE"`)
- **Service Kinds**: `["llm", "image", "stt"]`
- **Website**: `https://chatgpt.com/codex`
- **Executor**: `CodexExecutor` (`open-sse/executors/codex.js`)
- **Source Files**:
  - `open-sse/providers/registry/codex.js`
  - `open-sse/executors/codex.js`
  - `src/lib/oauth/providers/codex.js`
  - `open-sse/handlers/imageProviders/codex.js`
  - `open-sse/handlers/codexStt.js`

---

## 2. Endpoints by Service Kind & OAuth 2.0 Flow
1. **LLM Responses API**:
   - Standard: `POST https://chatgpt.com/backend-api/codex/responses`
   - Compact Mode (`body._compact === true`): `POST https://chatgpt.com/backend-api/codex/responses/compact`
   - Wire Format: `"openai-responses"` (`forceStream: true`)
2. **Image Generation (`open-sse/handlers/imageProviders/codex.js`)**:
   - `POST https://chatgpt.com/backend-api/codex/responses`
   - Uses `tools: [{ type: "image_generation", output_format, action, model, size, quality, background }]` over the Responses SSE endpoint (driving `gpt-5.6-terra` when invoking tool-level image models like `gpt-image-2.5`).
   - Rejects Free plan accounts (`resolveChatgptPlanType(creds) !== "free"`).
3. **Speech-to-Text (`open-sse/handlers/codexStt.js`)**:
   - `POST https://chatgpt.com/backend-api/transcribe`
   - Converts audio to WAV via `ffmpeg` (`convertAudio`, max 25 MiB / 120s, max 2 concurrent requests), sends `multipart/form-data` with field `file="audio.wav"` (`audio/wav`) and headers `Authorization: Bearer <accessToken>`, `ChatGPT-Account-Id: <accountId>`, `User-Agent: codex_cli_rs/0.159.0`.
4. **Usage & Rate-Limit Reset Credits**:
   - `url`: `https://chatgpt.com/backend-api/wham/usage`
   - `resetCreditsUrl`: `https://chatgpt.com/backend-api/wham/rate-limit-reset-credits`
   - `resetCreditsConsumeUrl`: `https://chatgpt.com/backend-api/wham/rate-limit-reset-credits/consume`
5. **OAuth 2.0 Authorization Code + PKCE (`S256`)**:
   - `clientId`: `app_EMoamEEZ73f0CkXaXp7hrann`
   - `authorizeUrl`: `https://auth.openai.com/oauth/authorize`
   - `tokenUrl`: `https://auth.openai.com/oauth/token` (`application/x-www-form-urlencoded`)
   - `scope`: `"openid profile email offline_access"`
   - `fixedPort`: `1455`, `callbackPath`: `"/auth/callback"`
   - `extraParams`: `{ id_token_add_organizations: "true", codex_cli_simplified_flow: "true", originator: "codex_cli_rs" }`
   - `refreshLeadMs`: `600000` (10 min), `maxRefreshAgeMs`: `691200000` (8 days)

---

## 3. Headers & Request Transformations (`CodexExecutor`)
- **Headers**:
  - `Authorization`: `Bearer <accessToken>`
  - `originator`: `"codex_cli_rs"`
  - `User-Agent`: `"codex_cli_rs/0.159.0"`
  - `version`: `"0.159.0"`
  - `session_id`: `<resolvedSessionId>` (derived from `rawHeaders`, `body`, `workspaceId`, `connectionId`)
  - `ChatGPT-Account-ID`: `<workspaceId || chatgptAccountId || accountId>`
  - `x-openai-internal-codex-responses-lite`: `"true"` (sent when `isCodexResponsesLiteModel(model)` is true and no `web_search` tool is present)
- **Request Transformations (`transformRequest`)**:
  - Converts `role: "system"` items in `body.input` to `role: "developer"`.
  - Strips server-generated item IDs matching `/^(rs|fc|resp|msg)_/` and `item_reference` items because `store: false` cannot resolve server-side IDs.
  - Normalizes function tools into flat Responses API tool objects (`{ type: "function", name, description, parameters, strict }`) and strips unsupported regex Unicode property escapes (`\p{...}`) via `stripCodexUnsupportedPatterns`.
  - **Responses Lite Mode (`responsesLite: true`)**: Moves `tools` and `instructions` into `body.input` prefix items (`{ type: "additional_tools", role: "developer", tools }` and `{ type: "message", role: "developer", content: [{ type: "input_text", text: instructions }] }`), sets `body.instructions = ""`, `body.tools = null`, `body.tool_choice ||= "auto"`, `body.parallel_tool_calls = false`, and `body.reasoning.context = "all_turns"`.
  - Forces `body.stream = true`, `body.store = false`, sets `body.include = ["reasoning.encrypted_content"]` when reasoning is enabled, and filters top-level keys against `RESPONSES_API_ALLOWLIST` (`model`, `input`, `instructions`, `tools`, `tool_choice`, `stream`, `store`, `reasoning`, `service_tier`, `include`, `prompt_cache_key`, `client_metadata`, `text`, `parallel_tool_calls`).
  - **Remote Image Prefetching (`prefetchImages`)**: Downloads remote `http(s)://` `image_url` entries and inlines them as `data:` base64 URIs in `{ type: "input_image", image_url, detail }`.
  - **SSE Peek & Retry (`_peekSseTransientError`)**: Peeks the first 256 KiB of 200 OK SSE streams to catch embedded `server_is_overloaded`, `service_unavailable_error`, or `model_at_capacity` errors before forwarding to the client.

---

## 4. Supported Models
| Model ID | Display Name | Kind | Flags / Upstream Mapping |
|---|---|---|---|
| `gpt-6.1-sol` | GPT 6.1 Sol | `llm` | `responsesLite: true` |
| `gpt-6-astra` | GPT 6.0 Astra | `llm` | — |
| `gpt-6-astra[1m]` | GPT 6.0 Astra (extended context) | `llm` | `upstreamModelId: "gpt-6-astra"` |
| `gpt-6-sol` | GPT 6.0 Sol | `llm` | `responsesLite: true` |
| `gpt-6-sol[1m]` | GPT 6.0 Sol (extended context) | `llm` | `upstreamModelId: "gpt-6-sol"`, `responsesLite: true` |
| `gpt-6-luna` | GPT 6.0 Luna | `llm` | `responsesLite: true` |
| `gpt-6-luna[1m]` | GPT 6.0 Luna (extended context) | `llm` | `upstreamModelId: "gpt-6-luna"`, `responsesLite: true` |
| `gpt-5.6-sol` | GPT 5.6 Sol | `llm` | (+ `[1m]` and `-review` variants) |
| `gpt-5.6-terra` | GPT 5.6 Terra | `llm` | (+ `[1m]` and `-review` variants) |
| `gpt-5.6-luna` | GPT 5.6 Luna | `llm` | (+ `[1m]` and `-review` variants) |
| `gpt-daybreak-blue-latest` | GPT Daybreak Blue | `llm` | — |
| `gpt-reserve` | GPT Reserve | `llm` | — |
| `codex-auto-review` | Codex Auto Review | `llm` | `quotaFamily: "review"` |
| `gpt-image-2.5` | GPT Image 2.5 | `image` | `text2img`, `edit`, `multiImage` |
| `gpt-image-2.5-flare` | GPT Image 2.5 Flare | `image` | `text2img`, `edit`, `multiImage` |
| `gpt-image-2.5-sunburst` | GPT Image 2.5 Sunburst | `image` | `text2img`, `edit`, `multiImage` |
| `gpt-image-2` | GPT Image 2 | `image` | `text2img`, `edit`, `multiImage` |
| `gpt-image-1.5` | GPT Image 1.5 | `image` | `text2img`, `edit`, `multiImage` |
| `gpt-5.6-sol-image` | GPT 5.6 Sol Image | `image` | `text2img`, `edit` |
| `gpt-5.6-terra-image` | GPT 5.6 Terra Image | `image` | `text2img`, `edit` |
| `gpt-5.6-luna-image` | GPT 5.6 Luna Image | `image` | `text2img`, `edit` |
| `transcribe` | Codex Transcription | `stt` | `responseFormats: ["json", "text"]` |

---

## 5. Verbatim Source Code Snippets

### `open-sse/providers/registry/codex.js` (Excerpt)
```javascript
const CODEX_CLI_VERSION = "0.159.0";
const GPT_6_LITE_THINKING_LEVELS = ["low", "medium", "high", "xhigh", "max"];

export default {
  id: "codex",
  priority: 30,
  alias: "cx",
  uiAlias: "cx",
  category: "oauth",
  transport: {
    baseUrl: "https://chatgpt.com/backend-api/codex/responses",
    format: "openai-responses",
    forceStream: true,
    cliVersion: CODEX_CLI_VERSION,
    headers: {
      originator: "codex_cli_rs",
      "User-Agent": `codex_cli_rs/${CODEX_CLI_VERSION}`,
      version: CODEX_CLI_VERSION,
    },
    usage: {
      url: "https://chatgpt.com/backend-api/wham/usage",
      resetCreditsUrl: "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits",
      resetCreditsConsumeUrl: "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits/consume",
    },
  },
  serviceKinds: ["llm","image","stt"],
  sttConfig: {
    baseUrl: "https://chatgpt.com/backend-api/transcribe",
    authType: "oauth",
    format: "codex-transcribe",
  },
  oauth: {
    clientId: "app_EMoamEEZ73f0CkXaXp7hrann",
    authorizeUrl: "https://auth.openai.com/oauth/authorize",
    tokenUrl: "https://auth.openai.com/oauth/token",
    scope: "openid profile email offline_access",
    codeChallengeMethod: "S256",
    fixedPort: 1455,
    callbackPath: "/auth/callback",
    extraParams: {
      id_token_add_organizations: "true",
      codex_cli_simplified_flow: "true",
      originator: "codex_cli_rs",
    },
    refreshLeadMs: 600000,
    refresh: {
      encoding: "form",
      scope: "openid profile email offline_access",
    },
    maxRefreshAgeMs: 691200000,
    trackRefreshAt: true,
  },
};
```

### `open-sse/executors/codex.js` (`buildHeaders` & `transformRequest` Excerpt)
```javascript
  buildHeaders(credentials, stream = true, _url = null, model = null, body = null) {
    const headers = super.buildHeaders(credentials, stream);
    if (isCodexResponsesLiteModel(model && getModelUpstreamId("cx", model)) && !body?.tools?.some?.(tool => tool?.type === "web_search")) {
      headers["x-openai-internal-codex-responses-lite"] = "true";
    }
    headers["session_id"] = this._currentSessionId || credentials?.connectionId || "default";
    if (!headers["originator"]) headers["originator"] = "codex_cli_rs";
    const accountId =
      credentials?.providerSpecificData?.workspaceId ||
      credentials?.providerSpecificData?.chatgptAccountId ||
      credentials?.providerSpecificData?.accountId;
    if (typeof accountId === "string" && accountId && !headers["ChatGPT-Account-ID"]) {
      headers["ChatGPT-Account-ID"] = accountId;
    }
    return headers;
  }
```
