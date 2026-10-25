# Vercel AI Gateway (`vercel` / `gateway`) Provider Specification (`fx`)

## 1. Overview
- **Provider ID**: `.gateway` (slug: `"vercel"`, aliases: `["gateway", "ai-gateway"]`)
- **Name**: `Vercel AI Gateway`
- **Source Files**:
  - `src/builtins/providers.zig`
  - `src/builtins/gateway.zig`
  - `src/gateway/client.zig`
  - `src/gateway/vercel_protocol.zig`
  - `src/gateway/vercel_model_policy.zig`
  - `src/gateway/vercel_failure_diagnostics.zig`
  - `src/builtins/gateway/permission_reviewer.zig`
  - `src/core/auth/oauth.zig`, `src/core/auth/credentials.zig`

## 2. Endpoints & Defaults
| Endpoint Purpose | HTTP Method & URL / Path | Notes |
| :--- | :--- | :--- |
| **Language Model Stream** | `POST https://ai-gateway.vercel.sh/v4/ai/language-model` | Overrideable via `FX_GATEWAY_CHAT_URL` (loopback HTTP only) |
| **Evaluation Model (Jev)** | `POST https://ai-gateway.vercel.sh/v4/ai/evaluation-model` | Used when `FX_REVIEW_MODEL=typesafeai/jev` without `TYPESAFE_API_KEY` |
| **Model Catalog** | `GET https://ai-gateway.vercel.sh/coding-agent/v1/models` | Appends `?teamId=<team>` for `.fx_login` credentials |
| **Credits Lookup** | `GET https://ai-gateway.vercel.sh/coding-agent/v1/credits` | Appends `?teamId=<team>` for `.fx_login` credentials |
| **OAuth Discovery & Token** | `https://vercel.com/.well-known/openid-configuration` | Device code & PKCE OAuth (`default_scope = "openid offline_access"`) |

- **Default Models (`src/builtins/gateway.zig`)**:
  - Default Agent Model: `spacexai/grok-4.7`
  - Default Title & Permission Reviewer Model: `openai/gpt-5.6-luna`

## 3. Authentication & Headers
- **Credential Sources**:
  1. `AI_GATEWAY_API_KEY` / `VERCEL_OIDC_TOKEN` (`.ai_gateway_api_key`, `.vercel_oidc`)
  2. `fx login` OAuth token (`.fx_login`, scope `"openid offline_access"`)
  3. Host-managed SDK credentials (`.host_managed`)
  - *Note*: Subscription credentials (`.chatgpt_subscription`, `.grok_subscription`) and `.configured` credentials are explicitly rejected before any Gateway HTTP request (`error.SubscriptionCredentialCannotAuthorizeGateway` / `error.ConfiguredCredentialCannotAuthorizeGateway`).
- **HTTP Headers (`src/gateway/client.zig`)**:
  - `Authorization: Bearer <token>`
  - `x-vercel-ai-gateway-team: <team>` (sent on language-model requests and non-`fx_login` catalog requests; for `fx_login` catalog and `/coding-agent/v1/credits`, `teamId` is passed as a URL query parameter instead)
  - `x-vercel-ai-gateway-model: <model>`
  - `x-vercel-ai-gateway-session-id: <session_id>` (when session ID is present)
  - `User-Agent: fx/<version>`
  - `http-referer` / `x-title` attribution headers (on model generation requests only)
  - Special case for `zai/glm-5.2`: embeds `"headers": {"user-agent": "fx/<version>"}` inside the JSON request body (`vercel_protocol.withRequestUserAgent`).

## 4. Request & Streaming Protocol (`src/gateway/vercel_protocol.zig`)
- **AI SDK v4 Language Model Wire Format**:
  - Serializes `prompt` array (`system`, `user`, `assistant`, `tool` messages), `tools` (`type: "function"`, `name`, `description`, `inputSchema`), `toolChoice` (`{"type": "auto" | "none" | "required"}` or specific tool), `maxOutputTokens`, and `providerOptions` (e.g., Anthropic thinking budgets, OpenAI `reasoningEffort` / `serviceTier: "priority" | "ultrafast"`).
- **Built-in Gateway Web Search Worker (`default_web_search_provider`)**:
  - Executes a private sub-request with `toolChoice: {"type": "required"}` and one of three Gateway-executed provider search tools (`FX_WEB_SEARCH_BACKEND`):
    1. `gateway.exa_search` (`name: "exa_search"`, `args: { numResults, includeDomains/excludeDomains, contents: { highlights: true } }`)
    2. `gateway.parallel_search` (`name: "parallel_search"`, `args: { mode: "one-shot", maxResults, sourcePolicy, excerpts: { maxCharsTotal } }`)
    3. `gateway.perplexity_search` (`name: "perplexity_search"`, `args: { maxResults, maxTokens, searchDomainFilter }`)

## 5. Source Implementation (`src/builtins/gateway.zig`)
```zig
pub const default_model = "spacexai/grok-4.7";
pub const title_model = "openai/gpt-5.6-luna";
pub const default_chat_url = "https://ai-gateway.vercel.sh/v4/ai/language-model";
pub const models_path = "/coding-agent/v1/models";
const credits_path = "/coding-agent/v1/credits";
pub const retry_count: usize = 3;
pub const chat_url_env = "FX_GATEWAY_CHAT_URL";
pub const default_model_catalog_base_url = "https://ai-gateway.vercel.sh";

pub const provider_bundle = provider_set.Bundle{
    .capabilities = .{ .gateway_prompt_caching = true, .fx_search = true, .vision_fallback = true },
    .presentation = provider_catalog.find(.gateway),
    .auth_strategy = .vercel,
    .title_model = title_model,
    .fallback_model_capabilities_fn = vercel_model_policy.capabilitiesForModel,
    .agent_stream = agent_stream_provider,
    .cli_model_catalog = cli_model_catalog_provider,
    .model_catalog = model_catalog_provider,
    .permission_reviewer = permission_reviewer.provider,
    .credits = credits_provider,
    .fx_search = default_web_search_provider,
};
```
