# Cohere (`cohere`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `cohere`
- **Package Entrypoints**:
  - `@opencode/ai/providers/cohere` (`packages/ai/src/providers/cohere.ts`)
  - `@opencode/ai/providers/cohere/chat` (`packages/ai/src/providers/cohere/chat.ts`)
- **Protocol**: `packages/ai/src/protocols/cohere-chat.ts` (`cohere-chat`)
- **Core Plugin**: `packages/core/src/plugin/provider/cohere.ts` (`opencode.provider.cohere`)

---

## 2. Endpoints & Authentication

| Route | Base URL | Path | Protocol |
|---|---|---|---|
| **Native v2 (`cohere-chat`)** | `https://api.cohere.com/v2` | `POST /chat` | `CohereChat.protocol` (SSE) |
| **Compatibility (`cohere-chat-completions`)** | `https://api.cohere.ai/compatibility/v1` | `POST /chat/completions` | `OpenAIChat.protocol` (`maxTokensField: "max_tokens"`, `reasoningField: "reasoning_content"`, `supportsStrictMode: false`) |

- **Authentication**: `Authorization: Bearer <apiKey>` (env fallback `COHERE_API_KEY`).

---

## 3. Native v2 Protocol (`packages/ai/src/protocols/cohere-chat.ts`)

```typescript
const Body = Schema.Struct({
  model: Schema.String,
  messages: Schema.Array(Message),
  stream: Schema.Literal(true),
  tools: Schema.optional(
    Schema.Array(
      Schema.Struct({
        type: Schema.Literal("function"),
        function: Schema.Struct({
          name: Schema.String,
          description: Schema.optional(Schema.String),
          parameters: JsonObject,
        }),
      }),
    ),
  ),
  tool_choice: Schema.optional(Schema.Literals(["NONE", "REQUIRED"])),
  thinking: Schema.optional(Schema.Struct({ type: Schema.String, token_budget: Schema.optional(Schema.Number) })),
  max_tokens: Schema.optional(Schema.Number),
  temperature: Schema.optional(Schema.Number),
  p: Schema.optional(Schema.Number),
  k: Schema.optional(Schema.Number),
  seed: Schema.optional(Schema.Number),
  stop_sequences: Schema.optional(Schema.Array(Schema.String)),
  frequency_penalty: Schema.optional(Schema.Number),
  presence_penalty: Schema.optional(Schema.Number),
})
```
- Maps `tool-plan-delta` (`delta.message.tool_plan`) to reasoning events with `providerMetadata: { cohere: { toolPlan: true } }` and round-trips `tool_plan` on assistant messages.
- Parses SSE event types: `message-start`, `content-start`, `content-delta`, `content-end`, `tool-plan-delta`, `tool-call-start`, `tool-call-delta`, `tool-call-end`, `message-end`.
