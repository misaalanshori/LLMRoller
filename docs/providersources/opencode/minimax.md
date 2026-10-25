# MiniMax (`minimax`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `minimax`
- **Catalog Host Aliases (`packages/core/src/aisdk-native.ts`)**: `minimax`, `minimax-cn`, `minimax-coding-plan`, `minimax-cn-coding-plan`
- **Package Entrypoints**:
  - `@opencode/ai/providers/minimax` (`packages/ai/src/providers/minimax.ts`)
  - `@opencode/ai/providers/minimax/chat` (`packages/ai/src/providers/minimax/chat.ts`)
  - `@opencode/ai/providers/minimax/messages` (`packages/ai/src/providers/minimax/messages.ts`)
  - `@opencode/ai/providers/minimax/responses` (`packages/ai/src/providers/minimax/responses.ts`)

---

## 2. Endpoints & Authentication (`packages/ai/src/providers/minimax.ts`)

| Route | Default Base URL | Path | Protocol & Headers |
|---|---|---|---|
| **Messages (`minimax-messages`, Default)** | `https://api.minimax.io/anthropic/v1` | `POST /messages` | `AnthropicMessages.protocol` (`anthropic-version: 2023-06-01`) |
| **Chat (`minimax-chat`)** | `https://api.minimax.io/v1` | `POST /chat/completions` | `minimax-chat` (`reasoning_split: true`, `supportsStrictMode: false`) |
| **Responses (`minimax-responses`)** | `https://api.minimax.io/v1` | `POST /responses` | `OpenResponses.protocol` |

- **Authentication**: `Authorization: Bearer <apiKey>` (env fallback `MINIMAX_API_KEY`).

---

## 3. Implementation (`packages/ai/src/providers/minimax.ts`)

```typescript
const chatProtocol = Protocol.make({
  id: "minimax-chat",
  body: {
    schema: Schema.Struct({
      ...OpenAIChat.bodyFields,
      thinking: ChatOptions.fields.thinking,
      reasoning_split: Schema.Boolean,
    }),
    from: Effect.fn("MiniMax.chatFromRequest")(function* (request: LLMRequest) {
      const options = yield* ProviderShared.validateWith(Schema.decodeUnknownEffect(ChatOptions))(
        request.providerOptions ?? {},
      )
      return {
        ...(yield* OpenAIChat.protocol.body.from(request)),
        thinking: options.thinking,
        // MiniMax otherwise embeds <think> tags in ordinary assistant text.
        reasoning_split: options.reasoningSplit ?? true,
      }
    }),
  },
  stream: OpenAIChat.protocol.stream,
})
```
