# vLLM (`vllm`) Local Provider Plugin Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `vllm`
- **Plugin ID**: `opencode.provider.vllm` (`packages/core/src/plugin/provider/vllm.ts`)
- **Helper**: `createLocalProviderPlugin` (`packages/core/src/plugin/provider/local.ts`)
- **Runtime Package**: `@opencode/ai/providers/openai-compatible`

---

## 2. Endpoints & Discovery Protocol

| Operation | Endpoint |
|---|---|
| **Default Origin** | `http://127.0.0.1:8000` |
| **Chat Completions Base URL** | `http://127.0.0.1:8000/v1` (`POST /chat/completions`) |
| **Health Probe** | `GET /health` |
| **Model Discovery** | `GET /v1/models` (filters `model.owned_by === "vllm"` and extracts `max_model_len`) |

---

## 3. Implementation (`packages/core/src/plugin/provider/vllm.ts`)

```typescript
const providerID = "vllm"

const RemoteModel = Schema.Struct({
  id: Schema.String,
  owned_by: Schema.String,
  max_model_len: Schema.NullOr(Schema.Int),
})

const Response = Schema.Struct({ data: Schema.Array(RemoteModel) })

export const make = createLocalProviderPlugin({
  id: "opencode.provider.vllm",
  providerID,
  name: "vLLM",
  origin: "http://127.0.0.1:8000",
  discover: (client) =>
    Effect.gen(function* () {
      yield* client.get("/health")
      const response = yield* client.getJson("/v1/models", Response)
      return response.data
        .filter((model) => model.owned_by === providerID && model.id.length > 0)
        .toSorted((a, b) => a.id.localeCompare(b.id))
    }),
  model: (item) => ({
    id: item.id,
    context: item.max_model_len,
  }),
})
```
