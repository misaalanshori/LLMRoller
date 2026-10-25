# Mistral AI (`mistral`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `mistral`
- **Package Entrypoint**: `@opencode/ai/providers/mistral` (`packages/ai/src/providers/mistral.ts`)
- **Protocol**: `packages/ai/src/protocols/mistral-chat.ts` (`mistral-chat`)
- **Tool ID Normalizer**: `packages/ai/src/protocols/utils/mistral-tool-id.js`

---

## 2. Endpoints & Authentication

| Parameter | Value |
|---|---|
| **Default Base URL** | `https://api.mistral.ai/v1` |
| **Chat Completions Path** | `POST /chat/completions` |
| **Authentication** | `Authorization: Bearer <apiKey>` (env fallback `MISTRAL_API_KEY`) |
| **Framing** | `Framing.sseWithDone` |

---

## 3. Protocol Implementation (`packages/ai/src/protocols/mistral-chat.ts`)

```typescript
export const MistralBody = Schema.Struct({
  model: Schema.String,
  messages: Schema.Array(MistralMessage),
  tools: optionalArray(MistralTool),
  tool_choice: Schema.optional(
    Schema.Union([
      Schema.Literals(["auto", "none", "any"]),
      Schema.Struct({ type: Schema.Literal("function"), function: Schema.Struct({ name: Schema.String }) }),
    ]),
  ),
  stream: Schema.Literal(true),
  max_tokens: Schema.optional(Schema.Number),
  random_seed: Schema.optional(Schema.Number),
  temperature: Schema.optional(Schema.Number),
  top_p: Schema.optional(Schema.Number),
  frequency_penalty: Schema.optional(Schema.Number),
  presence_penalty: Schema.optional(Schema.Number),
  stop: optionalArray(Schema.String),
  prompt_cache_key: Schema.optional(Schema.String),
  safe_prompt: Schema.optional(Schema.Boolean),
  document_image_limit: Schema.optional(Schema.Number),
  document_page_limit: Schema.optional(Schema.Number),
  parallel_tool_calls: Schema.optional(Schema.Boolean),
  reasoning_effort: Schema.optional(Schema.String),
  prompt_mode: Schema.optional(Schema.Literal("reasoning")),
})
```

### Key Mistral Quirks
- **9-Character Alphanumeric Tool Call IDs (`MistralToolID`)**: Normalizes all tool call IDs and tool result `tool_call_id`s to 9-character alphanumeric strings (`MistralToolID.normalizer`).
- **Assistant Prefix Flag**: Sets `prefix: true` when the final message in `request.messages` is an assistant message without tool calls.
- **Structured Thinking Content**: Preserves `{ type: "thinking", thinking: MistralThinkingUnit[] }` in `providerMetadata.mistral.thinking` and replays it on subsequent turns.
- **PDF Document Input**: Lowers `application/pdf` media parts to `{ type: "document_url", document_url: url }`.
