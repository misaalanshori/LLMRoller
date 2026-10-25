# Anthropic (`anthropic`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `anthropic`
- **Package Entrypoint**: `@opencode/ai/providers/anthropic` (`packages/ai/src/providers/anthropic.ts`)
- **Protocol**: `packages/ai/src/protocols/anthropic-messages.ts` (`anthropic-messages`)

---

## 2. Endpoints, Headers & Authentication

| Parameter | Value |
|---|---|
| **Default Base URL** | `https://api.anthropic.com/v1` |
| **Messages Path** | `POST /messages?beta=true` |
| **API Key Auth** | `x-api-key: <apiKey>` (env fallback `ANTHROPIC_API_KEY`) |
| **Auth Token Auth** | `Authorization: Bearer <authToken>` (`apiKey` and `authToken` are mutually exclusive) |
| **Static Headers** | `anthropic-version: 2023-06-01` |
| **Dynamic Beta Headers (`requiredBetaHeaders`)** | Always adds `interleaved-thinking-2025-05-14`; adds `compact-2026-01-12` when `context_management` or `compaction` blocks are present; adds `mid-conversation-output-config-2026-07-01` when mid-conversation `role: "system"` has `output_config`; adds `THINKING_BINDING_BETA` when `thinking.block_binding` is present |

---

## 3. Provider & Protocol Implementation (`packages/ai/src/providers/anthropic.ts` & `anthropic-messages.ts`)

```typescript
const auth = (options: ProviderAuthOption<"optional">) => {
  if ("auth" in options && options.auth) return options.auth
  return Auth.optional("apiKey" in options ? options.apiKey : undefined, "apiKey")
    .orElse(Auth.config("ANTHROPIC_API_KEY"))
    .pipe(Auth.header("x-api-key"))
}

export const route = Route.make({
  id: ADAPTER,
  provider: "anthropic",
  providerMetadataKey: "anthropic",
  protocol,
  endpoint: Endpoint.path((input) => (input.request.model.provider === "anthropic" ? `${PATH}?beta=true` : PATH), {
    baseURL: DEFAULT_BASE_URL,
  }),
  auth: Auth.none,
  transport: transport<AnthropicMessagesBody>(),
  headers: () => ({ "anthropic-version": "2023-06-01" }),
})
```
- **Prompt Caching**: Allocates up to 4 `cache_control: { type: "ephemeral", ttl?: "1h" }` breakpoints across `tools -> system -> messages`.
- **In-Band Compaction**: Supports `context_management.edits` (`type: "compact_20260112"`) and `compaction` content blocks (`compaction_delta` stream events).
- **Server Tools**: Round-trips `server_tool_use` and `web_search_tool_result`, `code_execution_tool_result`, `web_fetch_tool_result` blocks.
