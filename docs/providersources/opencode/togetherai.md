# Together AI (`togetherai`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `togetherai`
- **Package Entrypoint**: `@opencode/ai/providers/togetherai` (`packages/ai/src/providers/togetherai.ts`)
- **Protocol**: `OpenAIChat.protocol` (`packages/ai/src/protocols/openai-chat.ts`)

---

## 2. Endpoints, Authentication & Compatibility

| Parameter | Value |
|---|---|
| **Base URL** | `https://api.together.xyz/v1` |
| **Chat Completions Path** | `POST /chat/completions` |
| **Authentication** | `Authorization: Bearer <apiKey>` (env fallbacks: `TOGETHER_API_KEY`, `TOGETHER_AI_API_KEY`) |
| **Compatibility** | `{ maxTokensField: "max_tokens", supportsStrictMode: false }` |

---

## 3. Implementation (`packages/ai/src/providers/togetherai.ts`)

```typescript
export const id = ProviderID.make("togetherai")
const baseURL = "https://api.together.xyz/v1"

export const route = Route.make({
  id: "togetherai-chat",
  provider: id,
  providerMetadataKey: "togetherai",
  protocol: OpenAIChat.protocol,
  endpoint: Endpoint.path("/chat/completions", { baseURL }),
  framing: OpenAIChat.framing,
})

export const configure = (input: LanguageModelOptions = {}) => {
  const { apiKey: _apiKey, auth: _auth, baseURL: endpoint, ...defaults } = input
  const configured = route.with({
    ...defaults,
    endpoint: { baseURL: endpoint ?? baseURL },
    auth: AuthOptions.bearer(input, ["TOGETHER_API_KEY", "TOGETHER_AI_API_KEY"]),
  })
  return {
    id,
    model: (modelID: string | ModelID) =>
      configured.model<OpenAIProviderOptionsInput>({
        id: modelID,
        compatibility: { maxTokensField: "max_tokens", supportsStrictMode: false },
      }),
    configure,
  }
}
```
