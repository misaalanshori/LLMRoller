# Anthropic-Compatible (`anthropic-compatible`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `anthropic-compatible` (or custom `input.provider`)
- **Package Entrypoint**: `@opencode/ai/providers/anthropic-compatible` (`packages/ai/src/providers/anthropic-compatible.ts`)
- **Protocol**: `packages/ai/src/protocols/anthropic-messages.ts`

---

## 2. Endpoints & Authentication

| Parameter | Value |
|---|---|
| **Base URL** | Required (`input.baseURL`) |
| **Messages Path** | `POST /messages` (or `/messages?beta=true` when `provider === "anthropic"`) |
| **Authentication** | `x-api-key: <apiKey>` or `Authorization: Bearer <authToken>` |
| **Headers** | `anthropic-version: 2023-06-01` + dynamic `anthropic-beta` headers |

---

## 3. Implementation (`packages/ai/src/providers/anthropic-compatible.ts`)

```typescript
const compatibleRoute = AnthropicMessages.route.with({ id: "anthropic-compatible-messages", provider: id })
export const routes = [compatibleRoute]

const auth = (input: ProviderAuthOption<"optional">) => {
  if ("auth" in input && input.auth) return input.auth
  return Auth.optional("apiKey" in input ? input.apiKey : undefined, "apiKey").pipe(Auth.header("x-api-key"))
}

export const configure = (input: Config) => {
  const provider = input.provider ?? "anthropic-compatible"
  if (!input.baseURL)
    throw new ProviderConfigurationError({
      provider: ProviderID.make(provider),
      message: "Anthropic-compatible providers require a baseURL",
    })
  const { provider: _, baseURL, apiKey: _apiKey, auth: _auth, ...rest } = input
  const route = (provider === "anthropic" ? AnthropicMessages.route : compatibleRoute).with({
    ...rest,
    provider,
    endpoint: { baseURL },
    auth: auth(input),
  })
  return {
    id: ProviderID.make(provider),
    model: (modelID: string | ModelID) => route.model<AnthropicMessages.ProviderOptionsInput>({ id: modelID }),
    configure,
  }
}
```
