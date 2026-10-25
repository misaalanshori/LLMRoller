# OpenAI Codex (`codex`) Provider Specification (`fx`)

## 1. Overview
- **Provider ID**: `.codex` (slug: `"codex"`)
- **Name**: `Codex` (`Codex subscription`)
- **Description**: `ChatGPT Plus, Pro, Business, Enterprise, or Edu subscription`
- **Source Files**:
  - `src/builtins/providers.zig`
  - `src/core/auth/provider_catalog.zig`
  - `src/gateway/openai_codex.zig`
  - `src/gateway/openai_codex_models.zig`
  - `src/gateway/openai_codex_permission_reviewer.zig`
  - `src/gateway/responses_protocol.zig`
  - `src/core/auth/chatgpt_oauth.zig`
  - `src/usage/codex.zig`, `src/usage/usage.zig`
  - `sdk/chatgpt.js`, `sdk/chatgpt-sign-in.js`

## 2. Upstream Endpoints & Defaults
| Endpoint Purpose | HTTP Method & URL | Notes |
| :--- | :--- | :--- |
| **Responses Stream** | `POST https://chatgpt.com/backend-api/codex/responses` | Overrideable via `FX_E2E_OPENAI_CODEX_RESPONSES_URL` (loopback only) |
| **Model Catalog** | `GET https://chatgpt.com/backend-api/codex/models?client_version=<ver>` | Version resolved from `https://registry.npmjs.org/@openai/codex/latest` |
| **Usage Dashboard** | `GET https://chatgpt.com/backend-api/wham/usage` | Used by `/usage` (`src/usage/codex.zig`) |
| **OAuth Authorize / Token** | `https://auth.openai.com/oauth/authorize`, `POST https://auth.openai.com/oauth/token` | Client ID: `app_EMoamEEZ73f0CkXaXp7hrann` |

- **Default Title & Reviewer Model**: `gpt-5.6-luna` (`openai_codex_models.reviewer_model` & `title_model`).

## 3. Authentication & HTTP Headers
- **OAuth 2.0 PKCE (`src/core/auth/chatgpt_oauth.zig` & `sdk/chatgpt.js`)**:
  - **Client ID**: `app_EMoamEEZ73f0CkXaXp7hrann`
  - **Issuer**: `https://auth.openai.com`
  - **Token Endpoint**: `POST https://auth.openai.com/oauth/token`
  - **Scope**: `openid profile email offline_access api.connectors.read api.connectors.invoke`
  - **Local Callback Ports**: `1455`, `1457`
  - **Account ID Extraction**: Extracts `chatgpt_account_id` from the JWT claim `"https://api.openai.com/auth"`.
- **HTTP Headers (`extraHeaders` in `src/gateway/openai_codex.zig`)**:
  - `Authorization: Bearer <access_token>`
  - `chatgpt-account-id: <account_id>`
  - `originator: fx`
  - `OpenAI-Beta: responses=experimental`
  - `accept: text/event-stream`
  - `session-id: <session_id>` (when `session_id` is non-empty)
  - `x-client-request-id: <session_id>` (when `session_id` is non-empty)
  - `User-Agent: fx/<version>`

## 4. Request Payload & Protocol Quirks (`src/gateway/openai_codex.zig`)
- **Responses API Payload**:
  ```json
  {
    "model": "gpt-5.6-luna",
    "store": false,
    "stream": true,
    "instructions": "You are a helpful assistant.",
    "input": [...],
    "tools": [...],
    "tool_choice": "auto",
    "parallel_tool_calls": true,
    "include": ["reasoning.encrypted_content"],
    "service_tier": "priority",
    "text": { "verbosity": "low" },
    "reasoning": { "effort": "high", "summary": "auto" }
  }
  ```
- **Protocol Quirks**:
  - Rejects `max_output_tokens` (omitted intentionally because the ChatGPT Codex backend enforces its own output limit and rejects `max_output_tokens`).
  - Maps `"minimal"` reasoning effort to `"low"`.
  - Maps Fast mode (`request.provider_options.fast`) to `"service_tier": "priority"`.

## 5. Source Implementation (`src/gateway/openai_codex.zig`)
```zig
const endpoint = "https://chatgpt.com/backend-api/codex/responses";

pub fn extraHeaders(
    buf: *[max_extra_headers]std.http.Header,
    account_id: ?[]const u8,
    session_id: ?[]const u8,
) []std.http.Header {
    var count: usize = 0;
    if (account_id) |value| {
        buf[count] = .{ .name = "chatgpt-account-id", .value = value };
        count += 1;
    }
    buf[count] = .{ .name = "originator", .value = "fx" };
    count += 1;
    buf[count] = .{ .name = "OpenAI-Beta", .value = "responses=experimental" };
    count += 1;
    buf[count] = .{ .name = "accept", .value = "text/event-stream" };
    count += 1;
    if (session_id) |value| if (value.len > 0) {
        buf[count] = .{ .name = "session-id", .value = value };
        count += 1;
        buf[count] = .{ .name = "x-client-request-id", .value = value };
        count += 1;
    };
    return buf[0..count];
}
```
