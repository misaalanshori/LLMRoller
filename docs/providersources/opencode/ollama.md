# Ollama (`ollama`) Local Provider Plugin Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `ollama`
- **Plugin ID**: `opencode.provider.ollama` (`packages/core/src/plugin/provider/ollama.ts`)
- **Helper**: `createLocalProviderPlugin` (`packages/core/src/plugin/provider/local.ts`)
- **Runtime Package**: `@opencode/ai/providers/openai-compatible`

---

## 2. Endpoints & Two-Step Discovery Protocol

| Operation | Endpoint |
|---|---|
| **Default Origin** | `http://127.0.0.1:11434` |
| **Chat Completions Base URL** | `http://127.0.0.1:11434/v1` (`POST /chat/completions`) |
| **List Installed Models** | `GET /api/tags` |
| **Inspect Model Capabilities (Cached by `digest`)** | `POST /api/show` (`{ model: model.model }`, concurrency: 4) |

---

## 3. Implementation (`packages/core/src/plugin/provider/ollama.ts`)

```typescript
export const make = createLocalProviderPlugin({
  id: "opencode.provider.ollama",
  providerID: "ollama",
  name: "Ollama",
  origin: "http://127.0.0.1:11434",
  stripPathSuffix: /\/(?:v1|api)$/,
  discover: (client) =>
    Effect.gen(function* () {
      const cached = shows.get(client.root)
      const previous = cached && cached.apiKey === client.apiKey ? cached.entries : new Map()
      shows.set(client.root, { apiKey: client.apiKey, entries: previous })
      const response = yield* client.getJson("/api/tags", TagsResponse)
      const summaries = response.models
        .filter((model) => model.model.length > 0)
        .toSorted((a, b) => a.model.localeCompare(b.model))
      const entries = new Map<string, { digest: string; info: typeof ShowResponse.Type }>()
      const models = yield* Effect.forEach(
        summaries,
        (model) =>
          Effect.gen(function* () {
            const saved = previous.get(model.model)
            const info =
              saved?.digest === model.digest
                ? saved.info
                : yield* client.postJson("/api/show", ShowRequest, { model: model.model }, ShowResponse)
            entries.set(model.model, { digest: model.digest, info })
            return { ...model, show: info }
          }).pipe(Effect.orElseSucceed(() => undefined)),
        { concurrency: 4 },
      )
      shows.set(client.root, { apiKey: client.apiKey, entries })
      return models.filter(
        (model): model is DiscoveredModel =>
          model !== undefined && (model.show.capabilities?.includes("completion") ?? false),
      )
    }),
  model: (item) => ({
    id: item.model,
    name: item.name || item.model,
    family: item.show.details?.family || item.details.family,
    tools: item.show.capabilities?.includes("tools"),
    vision: item.show.capabilities?.includes("vision"),
    context: Object.entries(item.show.model_info ?? {}).flatMap(([key, value]) =>
      key.endsWith(".context_length") && typeof value === "number" && value > 0 ? [value] : [],
    )[0],
  }),
})
```
