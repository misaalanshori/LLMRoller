# OpenAI-Compatible (`openai-compatible`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `openai-compatible` (or custom `input.provider`)
- **Package Entrypoints**:
  - `@opencode/ai/providers/openai-compatible` (`packages/ai/src/providers/openai-compatible.ts`)
  - `@opencode/ai/providers/openai-compatible-responses` (`packages/ai/src/providers/openai-compatible-responses.ts`)
  - `@opencode/ai/providers/openai-compatible/responses` (`packages/ai/src/providers/openai-compatible/responses.ts`)
- **Protocols**:
  - `packages/ai/src/protocols/openai-compatible-chat.ts` (`openai-compatible-chat`)
  - `packages/ai/src/protocols/openai-compatible-responses.ts` (`openai-compatible-responses`)

---

## 2. Endpoints & Authentication

| Route | Base URL | Path | Protocol |
|---|---|---|---|
| **Chat (`openai-compatible-chat`)** | Required (`input.baseURL`) | `POST /chat/completions` | `OpenAICompatibleChat.protocol` |
| **Responses (`openai-compatible-responses`)** | Required (`input.baseURL`) | `POST /responses` | `OpenAICompatibleResponses.protocol` |

- **Authentication**: `Authorization: Bearer <apiKey>` (`AuthOptions.bearer(input, [])`).

---

## 3. Implementation (`packages/ai/src/providers/openai-compatible.ts` & `openai-compatible-responses.ts`)

```typescript
export const configure = (input: GenericModelOptions) => {
  const provider = input.provider ?? "openai-compatible"
  const { provider: _, baseURL, apiKey: _apiKey, auth: _auth, ...rest } = input
  const route = OpenAICompatibleChat.route.with({
    ...rest,
    provider,
    endpoint: { baseURL },
    auth: AuthOptions.bearer(input, []),
  })
  return {
    id: ProviderID.make(provider),
    model: (modelID: string | ModelID) => route.model<OpenAIProviderOptionsInput>({ id: modelID }),
    configure,
  }
}
```
