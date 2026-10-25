# Moonshot AI / Kimi (`moonshot`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `moonshotai`
- **Catalog Host Aliases (`packages/core/src/aisdk-native.ts`)**: `moonshotai`, `moonshotai-cn`, `kimi-for-coding`
- **Package Entrypoints**:
  - `@opencode/ai/providers/moonshot` (`packages/ai/src/providers/moonshot.ts`)
  - `@opencode/ai/providers/moonshot/chat` (`packages/ai/src/providers/moonshot/chat.ts`)
  - `@opencode/ai/providers/moonshot/messages` (`packages/ai/src/providers/moonshot/messages.ts`)
  - `@opencode/ai/providers/moonshot/responses` (`packages/ai/src/providers/moonshot/responses.ts`)

---

## 2. Endpoints & Authentication (`packages/ai/src/providers/moonshot.ts`)

| Route | Default Base URL | Path | Compatibility |
|---|---|---|---|
| **Chat (`moonshot-chat`, Default)** | `https://api.moonshot.ai/v1` | `POST /chat/completions` | `{ maxTokensField: "max_tokens", supportsStrictMode: false, supportsPromptCacheKey: true, sanitizer: "moonshot", reasoningField: "reasoning_content" }` |
| **Messages (`moonshot-messages`)** | `https://api.moonshot.ai/anthropic/v1` | `POST /messages` | `{ requireSignature: false, sanitizer: "moonshot" }` |
| **Responses (`moonshot-responses`)** | `https://api.moonshot.ai/v1` | `POST /responses` | `{ sanitizer: "moonshot" }` |

- **Authentication**: `Authorization: Bearer <apiKey>` (env fallbacks: `MOONSHOT_API_KEY`, `MOONSHOTAI_API_KEY`).

---

## 3. Implementation (`packages/ai/src/providers/moonshot.ts`)

```typescript
const chatProtocol = Protocol.make({
  id: "moonshot-chat",
  body: {
    schema: Schema.Struct({ ...OpenAIChat.bodyFields, thinking: ChatOptions.fields.thinking }),
    from: Effect.fn("Moonshot.chatFromRequest")(function* (request: LLMRequest) {
      const options = yield* ProviderShared.validateWith(Schema.decodeUnknownEffect(ChatOptions))(
        request.providerOptions ?? {},
      )
      return { ...(yield* OpenAIChat.protocol.body.from(request)), thinking: options.thinking }
    }),
  },
  stream: OpenAIChat.protocol.stream,
})
```
- Supports `thinking: { type: "enabled" | "disabled", keep?: "all" | null }` on `moonshot-chat` (for K2.6 and K2.7 Code).
- On `moonshot-messages` (and `api.kimi.com/coding`), `requireSignature: false` allows replaying unsigned thinking blocks as `{ type: "thinking", thinking: part.text, signature: "" }`.
