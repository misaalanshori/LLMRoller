# Fireworks AI (`fireworks`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `fireworks` (catalog alias `fireworks-ai`)
- **Package Entrypoint**: `@opencode/ai/providers/fireworks` (`packages/ai/src/providers/fireworks.ts`)
- **Protocol**: `OpenAIChat.protocol` (`packages/ai/src/protocols/openai-chat.ts`)

---

## 2. Endpoints, Authentication & Compatibility

| Parameter | Value |
|---|---|
| **Base URL** | `https://api.fireworks.ai/inference/v1` |
| **Chat Completions Path** | `POST /chat/completions` |
| **Authentication** | `Authorization: Bearer <apiKey>` (env fallback `FIREWORKS_API_KEY`) |
| **Compatibility** | `{ supportsPromptCacheKey: true }` |

---

## 3. Implementation (`packages/ai/src/providers/fireworks.ts`)

```typescript
export const id = ProviderID.make("fireworks")
const baseURL = "https://api.fireworks.ai/inference/v1"

export const route = Route.make({
  id: "fireworks-chat",
  provider: id,
  providerMetadataKey: "fireworks",
  protocol: OpenAIChat.protocol,
  endpoint: Endpoint.path("/chat/completions", { baseURL }),
  framing: OpenAIChat.framing,
})

export const configure = (input: LanguageModelOptions = {}) => {
  const { apiKey: _apiKey, auth: _auth, baseURL: endpoint, ...defaults } = input
  const configured = route.with({
    ...defaults,
    endpoint: { baseURL: endpoint ?? baseURL },
    auth: AuthOptions.bearer(input, "FIREWORKS_API_KEY"),
  })
  return {
    id,
    model: (modelID: string | ModelID) =>
      configured.model<OpenAIProviderOptionsInput>({ id: modelID, compatibility: { supportsPromptCacheKey: true } }),
    configure,
  }
}
```
