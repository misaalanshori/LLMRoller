# DigitalOcean Gradient AI (`digitalocean`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `digitalocean`
- **Package Entrypoint**: `@opencode/ai/providers/digitalocean` (`packages/ai/src/providers/digitalocean.ts`)
- **Core Plugin**: `packages/core/src/plugin/provider/digitalocean.ts` (`opencode.provider.digitalocean`)
- **Protocols**:
  - `digitalocean-chat` (`OpenAIChat` with Anthropic-style `cacheControl()` breakpoints)
  - `SystemOne.model` (`packages/ai/src/experimental/system-one.ts`)

---

## 2. Endpoints & Authentication

| Service | Endpoint |
|---|---|
| **Inference Base URL** | `https://inference.do-ai.run/v1` |
| **Chat Completions** | `POST https://inference.do-ai.run/v1/chat/completions` |
| **Experimental System One** | `POST https://inference.do-ai.run/v1/systemone` |
| **OAuth Implicit Authorize URL** | `https://cloud.digitalocean.com/v1/oauth/authorize` (`response_type=token`, `client_id: "b1a6c5158156caac821fd1b30253ca8acb52454a48fa744420e41889cb589f82"`, `redirect_uri: "http://localhost:1456/auth/callback"`, `scope: "genai:read inference:query"`) |
| **Model Routers Discovery** | `GET https://api.digitalocean.com/v2/gen-ai/models/routers` (`router:${router.name}`) |
| **API Key Auth** | `Authorization: Bearer <token>` (env fallbacks: `DIGITALOCEAN_ACCESS_TOKEN`, `DIGITALOCEAN_API_KEY`, `DO_INFERENCE_API_KEY`) |

---

## 3. Implementation (`packages/ai/src/providers/digitalocean.ts`)

```typescript
export const protocol = Protocol.make({
  id: "digitalocean-chat",
  body: {
    schema: OpenAIChat.protocol.body.schema,
    from: (request) => OpenAIChat.fromRequest(request, { cacheControl: cacheControl() }),
  },
  stream: OpenAIChat.protocol.stream,
})

export const configure = (input: LanguageModelOptions = {}) => {
  const { apiKey: _apiKey, auth: _auth, baseURL: endpoint, ...defaults } = input
  const auth = AuthOptions.bearer(input, ["DIGITALOCEAN_ACCESS_TOKEN", "DIGITALOCEAN_API_KEY", "DO_INFERENCE_API_KEY"])
  const configured = route.with({
    ...defaults,
    endpoint: { baseURL: endpoint ?? baseURL },
    auth,
  })
  const evaluation = (modelID: string | ModelID) =>
    SystemOne.model({
      id: modelID,
      provider: id,
      providerMetadataKey: "digitalocean",
      auth,
      baseURL: endpoint ?? baseURL,
      headers: input.headers,
      http: HttpOptions.make(input.http),
      rounding: SystemOne.jevRounding,
    })
  return {
    id,
    model: (modelID: string | ModelID) =>
      configured.model<OpenAIProviderOptionsInput>({
        id: modelID,
        compatibility: { supportsPromptCacheKey: true },
      }),
    experimental: { evaluation },
    configure,
  }
}
```
