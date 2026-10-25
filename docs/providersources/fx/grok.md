# xAI Grok (`grok`) Provider Specification (`fx`)

## 1. Overview
- **Provider ID**: `.grok` (slug: `"grok"`)
- **Name**: `Grok` (`Grok subscription`)
- **Description**: `SuperGrok or X Premium subscription`
- **Source Files**:
  - `src/builtins/providers.zig`
  - `src/core/auth/provider_catalog.zig`
  - `src/gateway/xai_grok.zig`
  - `src/gateway/xai_grok_models.zig`
  - `src/gateway/xai_grok_permission_reviewer.zig`
  - `src/gateway/responses_protocol.zig`
  - `src/core/auth/grok_oauth.zig`, `src/core/auth/grok_session.zig`
  - `src/usage/grok.zig`, `src/usage/usage.zig`
  - `sdk/grok.js`, `sdk/grok-sign-in.js`

## 2. Upstream Endpoints & Defaults
| Endpoint Purpose | HTTP Method & URL | Notes |
| :--- | :--- | :--- |
| **Responses Stream** | `POST https://cli-chat-proxy.grok.com/v1/responses` | Overrideable via `FX_E2E_XAI_GROK_RESPONSES_URL` (loopback only) |
| **Model Catalog** | `GET https://cli-chat-proxy.grok.com/v1/models` | Enriched with modalities from `GET https://api.x.ai/v1/language-models` |
| **CLI Version Compatibility** | `GET https://x.ai/cli/stable` | Resolved via `provider_versions.zig` for `x-grok-client-version` header |
| **Usage / Billing** | `GET https://cli-chat-proxy.grok.com/v1/billing?format=credits` & `/settings` | Used by `/usage` (`src/usage/grok.zig`, sends `x-userid: <account_id>`) |
| **OAuth Endpoints** | `https://auth.x.ai/oauth2/token`, `/oauth2/userinfo`, `/oauth2/revoke` | Client ID: `b1a00492-073a-47ea-816f-4c329264a828` |

- **Default Title & Reviewer Model**: `grok-4.5` (`xai_grok_models.title_model` & `xai_grok_permission_reviewer.reviewer_model`).

## 3. Authentication & HTTP Headers
- **OAuth 2.0 PKCE (`src/core/auth/grok_oauth.zig` & `sdk/grok.js`)**:
  - **Client ID**: `b1a00492-073a-47ea-816f-4c329264a828`
  - **Issuer**: `https://auth.x.ai`
  - **Scope**: `openid profile email offline_access grok-cli:access api:access`
  - **Account ID Verification**: Queries `GET https://auth.x.ai/oauth2/userinfo` (`Authorization: Bearer <access_token>`) and validates `info.sub` (`grok_session.validAccountId`).
- **HTTP Headers (`extraHeaders` in `src/gateway/xai_grok.zig`)**:
  - `Authorization: Bearer <access_token>`
  - `accept: text/event-stream`
  - `X-XAI-Token-Auth: xai-grok-cli` (when using subscription credentials)
  - `x-authenticateresponse: authenticate-response` (when using subscription credentials)
  - `x-grok-client-version: <version>` (resolved from `https://x.ai/cli/stable`)
  - `x-grok-client-identifier: fx`
  - `x-grok-model-override: <model>`
  - `x-grok-user-id: <account_id>`
  - `x-grok-conv-id: <session_id>` (when `session_id` is non-empty)
  - `User-Agent: fx/<version>`

## 4. Request Payload & Protocol Quirks (`src/gateway/xai_grok.zig`)
- **Responses API Payload**:
  ```json
  {
    "model": "grok-4.5",
    "store": false,
    "stream": true,
    "instructions": "You are a helpful assistant.",
    "input": [...],
    "tools": [...],
    "tool_choice": "auto",
    "parallel_tool_calls": true,
    "include": ["reasoning.encrypted_content"],
    "text": { "verbosity": "low" },
    "reasoning": { "effort": "high", "summary": "auto" },
    "service_tier": "priority",
    "max_output_tokens": 32768
  }
  ```
- Unlike `openai_codex`, `xai_grok` includes `"max_output_tokens"` when specified, and only emits `"tool_choice"` and `"parallel_tool_calls": true` when `tool_count > 0`.

## 5. Source Implementation (`src/gateway/xai_grok.zig`)
```zig
const endpoint = "https://cli-chat-proxy.grok.com/v1/responses";

pub fn extraHeaders(buf: *[max_extra_headers]std.http.Header, options: HeaderOptions) []std.http.Header {
    var count: usize = 0;
    buf[count] = .{ .name = "accept", .value = "text/event-stream" };
    count += 1;
    if (options.subscription) {
        buf[count] = .{ .name = "X-XAI-Token-Auth", .value = "xai-grok-cli" };
        count += 1;
        buf[count] = .{ .name = "x-authenticateresponse", .value = "authenticate-response" };
        count += 1;
    }
    if (options.client_version) |version| {
        buf[count] = .{ .name = "x-grok-client-version", .value = version };
        count += 1;
    }
    buf[count] = .{ .name = "x-grok-client-identifier", .value = "fx" };
    count += 1;
    buf[count] = .{ .name = "x-grok-model-override", .value = options.model };
    count += 1;
    if (options.account_id) |account_id| {
        buf[count] = .{ .name = "x-grok-user-id", .value = account_id };
        count += 1;
    }
    if (options.session_id) |session_id| if (session_id.len > 0) {
        buf[count] = .{ .name = "x-grok-conv-id", .value = session_id };
        count += 1;
    };
    return buf[0..count];
}
```
