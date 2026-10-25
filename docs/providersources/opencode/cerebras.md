# Cerebras (`cerebras`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `cerebras`
- **Package Entrypoint**: `@opencode/ai/providers/cerebras` (`packages/ai/src/providers/cerebras.ts`)
- **Core Plugin**: `packages/core/src/plugin/provider/cerebras.ts` (`opencode.provider.cerebras`)
- **Protocol**: `OpenAIChat.protocol` (`packages/ai/src/protocols/openai-chat.ts`)

---

## 2. Endpoints, Headers & Compatibility

| Parameter | Value |
|---|---|
| **Base URL** | `https://api.cerebras.ai/v1` |
| **Chat Completions Path** | `POST /chat/completions` |
| **Authentication** | `Authorization: Bearer <apiKey>` (env fallback `CEREBRAS_API_KEY`) |
| **Plugin Integration Header** | `X-Cerebras-3rd-Party-Integration: opencode` |
| **Model Compatibility** | `{ maxTokensField: "max_tokens", reasoningField: "reasoning", supportsPromptCacheKey: true }` |

---

## 3. Implementation (`packages/ai/src/providers/cerebras.ts` & `packages/core/src/plugin/provider/cerebras.ts`)

```typescript
export const configure = (input: LanguageModelOptions = {}) => {
  const { apiKey: _apiKey, auth: _auth, baseURL: endpoint, ...defaults } = input
  const configured = route.with({
    ...defaults,
    endpoint: { baseURL: endpoint ?? baseURL },
    auth: AuthOptions.bearer(input, "CEREBRAS_API_KEY"),
  })
  return {
    id,
    model: (modelID: string | ModelID) =>
      configured.model<OpenAIProviderOptionsInput>({
        id: modelID,
        compatibility: {
          maxTokensField: "max_tokens",
          reasoningField: "reasoning",
          supportsPromptCacheKey: true,
        },
      }),
    configure,
  }
}
```

```typescript
export const CerebrasPlugin = define({
  id: "opencode.provider.cerebras",
  effect: Effect.fn(function* (ctx) {
    yield* ctx.provider.transform((evt) => {
      for (const item of evt.list()) {
        if (item.provider.package !== "@opencode/ai/providers/cerebras") continue
        evt.update(item.provider.id, (provider) => {
          provider.headers = { ...provider.headers, "X-Cerebras-3rd-Party-Integration": "opencode" }
        })
      }
    })
  }),
})
```
