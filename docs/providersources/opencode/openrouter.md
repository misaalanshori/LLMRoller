# OpenRouter (`openrouter`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `openrouter`
- **Package Entrypoint**: `@opencode/ai/providers/openrouter` (`packages/ai/src/providers/openrouter.ts`)
- **Core Plugin**: `packages/core/src/plugin/provider/openrouter.ts` (`opencode.provider.openrouter`)
- **Protocols**:
  - `openrouter-chat` (extends `OpenAIChat.protocol` with `cacheControl()`)
  - `SystemOne.model` (`packages/ai/src/experimental/system-one.ts`)

---

## 2. Endpoints, Headers & Authentication

| Service | Endpoint |
|---|---|
| **Base URL** | `https://openrouter.ai/api/v1` |
| **Chat Completions** | `POST https://openrouter.ai/api/v1/chat/completions` |
| **Experimental System One** | `POST https://openrouter.ai/api/v1/systemone` |
| **Authentication** | `Authorization: Bearer <apiKey>` (env fallback `OPENROUTER_API_KEY`) |
| **Plugin Headers (`OpenRouterPlugin`)** | `HTTP-Referer: https://opencode.ai/`, `X-Title: opencode` |
| **Legacy Native Rewrite Headers (`aisdk-native.ts`)** | `appName` -> `X-OpenRouter-Title`, `appUrl` -> `HTTP-Referer`, `api_keys` -> `X-Provider-API-Keys: JSON.stringify(api_keys)` |

---

## 3. Request Body Construction & Reasoning Normalization (`packages/ai/src/providers/openrouter.ts`)

```typescript
export const protocol = Protocol.make({
  id: "openrouter-chat",
  body: {
    schema: OpenRouterBody,
    from: (request) =>
      OpenAIChat.fromRequest(request, {
        cacheControl: cacheControl(),
        assistant: (source, message) => {
          if (message.role !== "assistant") return message
          const reasoning = source.content
            .filter((part) => part.type === "reasoning")
            .map((part) => part.text)
            .join("")
          const reasoningDetails = Array.isArray(message.reasoning_details) ? message.reasoning_details : undefined
          return {
            ...message,
            reasoning_content: undefined,
            reasoning_text: undefined,
            reasoning: reasoning && reasoningDetails && reasoningDetails.length > 0 ? reasoning : undefined,
            reasoning_details: reasoningDetails,
          }
        },
      }).pipe(Effect.map((body) => bodyOptions(body, request.providerOptions, request.generation?.maxTokens))),
  },
  stream: OpenAIChat.protocol.stream,
})
```
- Automatically sets `usage: { include: true }` by default.
- Fits `reasoning.max_tokens` below `maxTokens` with a minimum of `1,024` tokens (`ProviderShared.fitThinkingBudget`).
- Disables `gpt-5-chat-latest` and `openai/gpt-5-chat` in `OpenRouterPlugin`.
