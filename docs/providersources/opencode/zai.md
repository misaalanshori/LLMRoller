# Z.ai (`zai`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `zai` (catalog alias `zhipuai`)
- **Package Entrypoints**:
  - `@opencode/ai/providers/zai` (`packages/ai/src/providers/zai.ts`)
  - `@opencode/ai/providers/zai/chat` (`packages/ai/src/providers/zai/chat.ts`)
- **Protocols**:
  - `packages/ai/src/protocols/zai-chat.ts` (`zai-chat`)
  - `packages/ai/src/protocols/zai-images.ts` (`zai-images`)

---

## 2. Endpoints & Authentication

| Service | Endpoint |
|---|---|
| **Base URL** | `https://api.z.ai/api/paas/v4` |
| **Chat Completions (`zai-chat`)** | `POST /chat/completions` |
| **Image Generation (`zai-images`)** | `POST /images/generations` |
| **Authentication** | `Authorization: Bearer <apiKey>` (env fallback `ZAI_API_KEY`) |
| **Compatibility** | `{ maxTokensField: "max_tokens", supportsStrictMode: false, reasoningField: "reasoning_content", zaiToolStream: false }` |

---

## 3. Protocol Implementation (`packages/ai/src/protocols/zai-chat.ts`)

```typescript
// Tool streaming was introduced in GLM-4.6; later versions inherit support.
const supportsToolStreaming = (modelID: string) => {
  const match = /(?:^|\/)glm-(\d+)(?:\.(\d+))?(?:-|$)/i.exec(modelID)
  return match !== null && (Number(match[1]) > 4 || (Number(match[1]) === 4 && Number(match[2] ?? 0) >= 6))
}

const fromRequest = Effect.fn("ZAIChat.fromRequest")(function* (request: LLMRequest) {
  const options = yield* ProviderShared.validateWith(Schema.decodeUnknownEffect(Options))(request.providerOptions ?? {})
  const body = yield* OpenAIChat.protocol.body.from(request)
  return {
    ...body,
    thinking: options.thinking,
    tool_stream:
      options.toolStream ?? (body.tools?.length && supportsToolStreaming(request.model.id) ? true : undefined),
    do_sample: options.doSample,
    response_format: options.responseFormat,
    request_id: options.requestID,
    user_id: options.userID,
  }
})
```
- Automatically enables `tool_stream: true` on `glm-4.6+` / `glm-5+` whenever `tools` are present.
