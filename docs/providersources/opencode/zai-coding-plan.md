# Z.ai Coding Plan (`zai-coding-plan`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `zai-coding-plan` (catalog alias `zhipuai-coding-plan`)
- **Package Entrypoints**:
  - `@opencode/ai/providers/zai-coding-plan` (`packages/ai/src/providers/zai-coding-plan.ts`)
  - `@opencode/ai/providers/zai-coding-plan/chat` (`packages/ai/src/providers/zai-coding-plan/chat.ts`)
  - `@opencode/ai/providers/zai-coding-plan/messages` (`packages/ai/src/providers/zai-coding-plan/messages.ts`)
  - `@opencode/ai/providers/zai-coding-plan/responses` (`packages/ai/src/providers/zai-coding-plan/responses.ts`)
- **Protocols**:
  - `ZAIChat.protocol` (`packages/ai/src/protocols/zai-chat.ts`)
  - `ZAIMessages.protocol` (`packages/ai/src/protocols/zai-messages.ts`)
  - `OpenResponses.protocol`

---

## 2. Endpoints & Authentication (`packages/ai/src/providers/zai-coding-plan.ts`)

| Route | Default Base URL | Path | Protocol & Compatibility |
|---|---|---|---|
| **Chat (`zai-coding-chat`, Default)** | `https://api.z.ai/api/coding/paas/v4` | `POST /chat/completions` | `ZAIChat.protocol` (`ZAIChat.compatibility`) |
| **Messages (`zai-coding-messages`)** | `https://api.z.ai/api/anthropic/v1` | `POST /messages` | `ZAIMessages.protocol` (`anthropic-version: 2023-06-01`, `requireSignature: false`) |
| **Responses (`zai-coding-responses`)** | `https://api.z.ai/api/v1` | `POST /responses` | `OpenResponses.protocol` |

- **Authentication**: `Authorization: Bearer <apiKey>` (env fallback `ZAI_API_KEY`).

---

## 3. Implementation (`packages/ai/src/providers/zai-coding-plan.ts`)

```typescript
const chatRoute = Route.make({
  id: "zai-coding-chat",
  provider: id,
  providerMetadataKey: "zai",
  protocol: ZAIChat.protocol,
  endpoint: Endpoint.path("/chat/completions", { baseURL: "https://api.z.ai/api/coding/paas/v4" }),
  framing: OpenAIChat.framing,
})
const messagesRoute = Route.make({
  id: "zai-coding-messages",
  provider: id,
  providerMetadataKey: "zai",
  protocol: ZAIMessages.protocol,
  endpoint: Endpoint.path("/messages", { baseURL: "https://api.z.ai/api/anthropic/v1" }),
  framing: AnthropicMessages.framing,
  headers: () => ({ "anthropic-version": "2023-06-01" }),
})
const responsesRoute = Route.make({
  id: "zai-coding-responses",
  provider: id,
  providerMetadataKey: "zai",
  protocol: OpenResponses.protocol,
  endpoint: Endpoint.path("/responses", { baseURL: "https://api.z.ai/api/v1" }),
  framing: Framing.sse,
})
```
