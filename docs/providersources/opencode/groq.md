# Groq (`groq`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `groq`
- **Package Entrypoint**: `@opencode/ai/providers/groq` (`packages/ai/src/providers/groq.ts`)
- **Protocol**: `groq-chat` (extends `OpenAIChat.protocol`)

---

## 2. Endpoints, Authentication & Compatibility

| Parameter | Value |
|---|---|
| **Base URL** | `https://api.groq.com/openai/v1` |
| **Chat Completions Path** | `POST /chat/completions` |
| **Authentication** | `Authorization: Bearer <apiKey>` (env fallback `GROQ_API_KEY`) |
| **Compatibility** | `{ maxTokensField: "max_completion_tokens", reasoningField: "reasoning", requireReasoning: false, supportsStrictMode: false }` |

---

## 3. Protocol & Provider Implementation (`packages/ai/src/providers/groq.ts`)

```typescript
export const protocol = Protocol.make({
  id: "groq-chat",
  body: {
    schema: Schema.Struct({
      ...OpenAIChat.bodyFields,
      reasoning_format: Schema.optional(Schema.Literal("parsed")),
      include_reasoning: Schema.optional(Schema.Boolean),
      parallel_tool_calls: Schema.optional(Schema.Boolean),
      service_tier: Schema.optional(Schema.String),
      user: Schema.optional(Schema.String),
    }),
    from: Effect.fn("Groq.fromRequest")(function* (request: LLMRequest) {
      const options = yield* ProviderShared.validateWith(Schema.decodeUnknownEffect(Options))(
        request.providerOptions ?? {},
      )
      const gptOSS = request.model.id.startsWith("openai/gpt-oss-")
      return {
        ...(yield* OpenAIChat.fromRequest(request)),
        reasoning_format: gptOSS ? undefined : ("parsed" as const),
        include_reasoning: gptOSS ? options.includeReasoning : undefined,
        parallel_tool_calls: options.parallelToolCalls,
        service_tier: options.serviceTier,
        user: options.user,
      }
    }),
  },
  stream: OpenAIChat.protocol.stream,
})
```
- For `openai/gpt-oss-*` models, sends `include_reasoning` instead of `reasoning_format: "parsed"`.
- For all other models on Groq, sets `reasoning_format: "parsed"`.
