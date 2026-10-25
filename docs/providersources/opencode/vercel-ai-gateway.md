# Vercel AI Gateway (`vercel-ai-gateway`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `vercel-ai-gateway`
- **Package Entrypoints**:
  - `@opencode/ai/providers/vercel-ai-gateway` (`packages/ai/src/providers/vercel-ai-gateway.ts`)
  - `@opencode/ai/providers/vercel-ai-gateway/chat` (`packages/ai/src/providers/vercel-ai-gateway/chat.ts`)
  - `@opencode/ai/providers/vercel-ai-gateway/messages` (`packages/ai/src/providers/vercel-ai-gateway/messages.ts`)
  - `@opencode/ai/providers/vercel-ai-gateway/responses` (`packages/ai/src/providers/vercel-ai-gateway/responses.ts`)
- **Gateway Protocol Helper**: `packages/ai/src/protocols/utils/gateway-protocol.ts`
- **Core Plugin**: `packages/core/src/plugin/provider/vercel.ts` (`opencode.provider.vercel`)

---

## 2. Endpoints, Multi-API Model Routing & Headers (`packages/ai/src/providers/vercel-ai-gateway.ts`)

| Route | Base URL & Path | Protocol |
|---|---|---|
| **Messages (`vercel-ai-gateway-messages`)** | `POST https://ai-gateway.vercel.sh/v1/messages` | `AnthropicMessages.protocol` (`anthropic-version: 2023-06-01`, `requireSignature: modelID.startsWith("anthropic/")`) |
| **Responses (`vercel-ai-gateway-responses`)** | `POST https://ai-gateway.vercel.sh/v1/responses` | `OpenResponses.protocol` (`store: false`, `include: ["reasoning.encrypted_content"]`) |
| **Chat (`vercel-ai-gateway-chat`)** | `POST https://ai-gateway.vercel.sh/v1/chat/completions` | `OpenAIChat.protocol` (`reasoningField: "reasoning"`) |
| **Images** | `POST https://ai-gateway.vercel.sh/v1/images/generations` (or `/edits`) | `OpenAIImages.protocol` |
| **Experimental Evaluation** | `POST https://ai-gateway.vercel.sh/v1/evaluate` | `vercel-evaluation` |

- **Authentication**: `Authorization: Bearer <token>` (env fallbacks: `AI_GATEWAY_API_KEY`, `VERCEL_OIDC_TOKEN`).
- **Session Affinity Header**: When `request.promptCacheKey` is set, sends `x-session-affinity: <promptCacheKey>`.
- **Default Family Routing (`model(modelID)`)**:
  - `/^(openai\/gpt-|spacexai\/grok-)/.test(modelID)` -> `responses(modelID)`
  - `modelID.startsWith("meta/muse-")` -> `chat(modelID)`
  - All other models -> `messages(modelID)`

```typescript
const model = (modelID: string | ModelID) => {
  if (/^(openai\/gpt-|spacexai\/grok-)/.test(modelID)) return responses(modelID)
  if (modelID.startsWith("meta/muse-")) return chat(modelID)
  return messages(modelID)
}
```
