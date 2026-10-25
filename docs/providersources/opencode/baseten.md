# Baseten (`baseten`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `baseten`
- **Package Entrypoint**: `@opencode/ai/providers/baseten` (`packages/ai/src/providers/baseten.ts`)
- **Protocols**:
  - `OpenAIChat.protocol` (`packages/ai/src/protocols/openai-chat.ts`)
  - `SystemOne.model` (`packages/ai/src/experimental/system-one.ts`)

---

## 2. Endpoints & Authentication

| Service | Endpoint |
|---|---|
| **Base URL** | `https://inference.baseten.co/v1` |
| **Chat Completions** | `POST https://inference.baseten.co/v1/chat/completions` |
| **Experimental System One Evaluation** | `POST https://inference.baseten.co/v1/systemone` |
| **Authentication** | `Authorization: Bearer <apiKey>` (env fallback `BASETEN_API_KEY`) |

---

## 3. Implementation (`packages/ai/src/providers/baseten.ts`)

```typescript
export const id = ProviderID.make("baseten")
const baseURL = "https://inference.baseten.co/v1"

export const route = Route.make({
  id: "baseten-chat",
  provider: id,
  providerMetadataKey: "baseten",
  protocol: OpenAIChat.protocol,
  endpoint: Endpoint.path("/chat/completions", { baseURL }),
  framing: OpenAIChat.framing,
})

export const configure = (input: LanguageModelOptions = {}) => {
  const { apiKey: _apiKey, auth: _auth, baseURL: endpoint, ...defaults } = input
  const auth = AuthOptions.bearer(input, "BASETEN_API_KEY")
  const configured = route.with({
    ...defaults,
    endpoint: { baseURL: endpoint ?? baseURL },
    auth,
  })
  const evaluation = (modelID: string | ModelID) =>
    SystemOne.model<BasetenEvaluationOptions>({
      id: modelID,
      provider: id,
      providerMetadataKey: "baseten",
      auth,
      baseURL: endpoint ?? baseURL,
      headers: input.headers,
      http: HttpOptions.make(input.http),
    })
  return {
    id,
    model: (modelID: string | ModelID) => configured.model<OpenAIProviderOptionsInput>({ id: modelID }),
    experimental: { evaluation },
    configure,
  }
}
```
