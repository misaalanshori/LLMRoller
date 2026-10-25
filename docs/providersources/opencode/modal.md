# Modal (`modal`) Provider Plugin Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `modal`
- **Plugin ID**: `opencode.provider.modal` (`packages/core/src/plugin/provider/modal.ts`)
- **Model Catalog Sync**: `packages/core/src/modal/models.ts` (`ModalModels`)
- **Runtime Package**: `@opencode/ai/providers/openai-compatible`

---

## 2. Endpoints & Authentication

| Parameter | Value |
|---|---|
| **Base URL** | Configured `provider.settings.baseURL` |
| **Live Models Discovery** | `GET ${baseURL}/models` (3s timeout) |
| **Authentication** | `Authorization: Bearer <apiKey>` (env fallback `MODAL_PROXY_TOKEN`) |

---

## 3. Dynamic Model Catalog Derivation (`packages/core/src/modal/models.ts`)

```typescript
export async function load(baseURL: string, apiKey: string): Promise<Snapshot> {
  const response = await fetch(`${baseURL.replace(/\/+$/, "")}/models`, {
    headers: {
      Authorization: `Bearer ${apiKey}`,
    },
    signal: AbortSignal.timeout(3_000),
  })
  if (!response.ok) throw new Error(`Failed to fetch Modal models: ${response.status}`)

  return decodeResponse(await response.json()).data.flatMap((raw) => {
    const model = Option.getOrUndefined(decodeModel(raw))
    return model ? [model] : []
  })
}
```
- Maps `pricing.prompt`, `pricing.completion`, and `pricing.input_cache_read` (multiplied by `1_000_000`) to `USDPerMillionTokens`.
- Maps `interleaved` (`true` or `{ field: "reasoning" | "reasoning_content" | "reasoning_details" }`) to model compatibility and `reasoning_options` (`type: "effort"`) to model variants.
