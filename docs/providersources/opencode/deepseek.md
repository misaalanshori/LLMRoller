# DeepSeek (`deepseek`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `deepseek`
- **Package Entrypoint**: `@opencode/ai/providers/deepseek` (`packages/ai/src/providers/deepseek.ts`)
- **Protocol**: `OpenAIChat.protocol` (`packages/ai/src/protocols/openai-chat.ts`)

---

## 2. Endpoints, Authentication & Compatibility

| Parameter | Value |
|---|---|
| **Base URL** | `https://api.deepseek.com/v1` |
| **Chat Completions Path** | `POST /chat/completions` |
| **Authentication** | `Authorization: Bearer <apiKey>` (env fallback `DEEPSEEK_API_KEY`) |
| **Compatibility** | `{ maxTokensField: "max_tokens" }` |

---

## 3. Implementation (`packages/ai/src/providers/deepseek.ts`)

```typescript
export const id = ProviderID.make("deepseek")
const baseURL = "https://api.deepseek.com/v1"

export const route = Route.make({
  id: "deepseek-chat",
  provider: id,
  providerMetadataKey: "deepseek",
  protocol: OpenAIChat.protocol,
  endpoint: Endpoint.path("/chat/completions", { baseURL }),
  framing: OpenAIChat.framing,
})

export const configure = (input: LanguageModelOptions = {}) => {
  const { apiKey: _apiKey, auth: _auth, baseURL: endpoint, ...defaults } = input
  const configured = route.with({
    ...defaults,
    endpoint: { baseURL: endpoint ?? baseURL },
    auth: AuthOptions.bearer(input, "DEEPSEEK_API_KEY"),
  })
  return {
    id,
    model: (modelID: string | ModelID) =>
      configured.model<OpenAIProviderOptionsInput>({
        id: modelID,
        compatibility: { maxTokensField: "max_tokens" },
      }),
    configure,
  }
}
```
