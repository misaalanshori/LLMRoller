# Replicate (`replicate`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `replicate`
- **Package Entrypoint**: `packages/ai/src/providers/replicate.ts`
- **Protocol**: `packages/ai/src/protocols/replicate-images.ts` (`replicate-images`)

---

## 2. Endpoints & Authentication

| Operation | Endpoint |
|---|---|
| **Default Base URL** | `https://api.replicate.com` |
| **Start Official Model Prediction (`owner/name`)** | `POST /v1/models/${request.model.id}/predictions` (`{ input }`) |
| **Start Versioned Prediction (`owner/name:version` or version ID)** | `POST /v1/predictions` (`{ version: model, input }`) |
| **Poll Status & Result** | `GET ${token.getURL}` (`prediction.urls.get`) |
| **Cancel Prediction** | `POST ${token.cancelURL}` (`prediction.urls.cancel`) |
| **Authentication** | `Authorization: Bearer <token>` (env fallback `REPLICATE_API_TOKEN`) |

---

## 3. Implementation (`packages/ai/src/protocols/replicate-images.ts`)

```typescript
// Official models run by `owner/name`; anything else (`owner/name:version` or a bare version id) is a pinned version.
const isOfficial = (model: string) => /^[^/:]+\/[^/:]+$/.test(model)

const fromRequest = Effect.fn("ReplicateImages.fromRequest")(function* (request: Request) {
  const native = yield* Effect.forEach(Object.entries(request.providerOptions ?? {}), ([key, value]) =>
    inputValue(value).pipe(Effect.map((lowered) => [key, lowered] as const)),
  )
  const input = mergeJsonRecords({ prompt: request.prompt }, Object.fromEntries(native))
  const model = request.model.id
  return MediaProtocol.json(
    mergeJsonRecords(isOfficial(model) ? { input } : { version: model, input }, request.http?.body) ?? {},
  )
})
```
- Supports `Prefer: wait=60` header to hold the submission open until the prediction finishes.
- Enforces a 256 KB limit (`MAX_DATA_URL_BYTES`) on inline data URL inputs and sets a 1-hour expiry (`OUTPUT_RETENTION`) on output URLs.
