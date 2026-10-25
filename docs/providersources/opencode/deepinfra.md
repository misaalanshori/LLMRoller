# DeepInfra (`deepinfra`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `deepinfra`
- **Package Entrypoint**: `@opencode/ai/providers/deepinfra` (`packages/ai/src/providers/deepinfra.ts`)
- **Protocol**: `OpenAIChat.protocol` (`packages/ai/src/protocols/openai-chat.ts`)

---

## 2. Endpoints, Authentication & Compatibility

| Parameter | Value |
|---|---|
| **Default Base URL** | `https://api.deepinfra.com/v1/openai` (automatically appends `/openai` if custom `baseURL` omits it) |
| **Chat Completions Path** | `POST /chat/completions` |
| **Authentication** | `Authorization: Bearer <apiKey>` (env fallback `DEEPINFRA_API_KEY`) |
| **Compatibility** | `{ maxTokensField: "max_tokens", reasoningField: "reasoning_content", supportsPromptCacheKey: true }` |

---

## 3. Implementation (`packages/ai/src/providers/deepinfra.ts`)

```typescript
export const id = ProviderID.make("deepinfra")
const baseURL = "https://api.deepinfra.com/v1/openai"

export const configure = (input: LanguageModelOptions = {}) => {
  const { apiKey: _apiKey, auth: _auth, baseURL: endpoint, ...defaults } = input
  const root = endpoint?.replace(/\/+$/, "")
  const configured = route.with({
    ...defaults,
    endpoint: {
      baseURL: root === undefined ? baseURL : root.endsWith("/openai") ? root : `${root}/openai`,
    },
    auth: AuthOptions.bearer(input, "DEEPINFRA_API_KEY"),
  })
  return {
    id,
    model: (modelID: string | ModelID) =>
      configured.model<OpenAIProviderOptionsInput>({
        id: modelID,
        compatibility: {
          maxTokensField: "max_tokens",
          reasoningField: "reasoning_content",
          supportsPromptCacheKey: true,
        },
      }),
    configure,
  }
}
```
