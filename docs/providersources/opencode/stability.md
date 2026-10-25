# Stability AI (`stability`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `stability`
- **Package Entrypoint**: `packages/ai/src/providers/stability.ts`
- **Protocol**: `packages/ai/src/protocols/stability-images.ts` (`stability-images`, `stability-upscale`)

---

## 2. Endpoints, Headers & Authentication

| Operation | Endpoint |
|---|---|
| **Default Base URL** | `https://api.stability.ai` |
| **Image Generation / Edit** | `POST /v2beta/stable-image/generate/${endpoint(request.model.id)}` (`sd3.5-*` -> `/sd3`, `ultra` -> `/ultra`, `core` -> `/core`) |
| **Creative Upscale Start** | `POST /v2beta/stable-image/upscale/creative` |
| **Creative Upscale Poll & Result** | `GET /v2beta/results/${token.id}` (HTTP 202 = running, HTTP 200 = completed) |
| **Authentication** | `Authorization: Bearer <apiKey>` (env fallback `STABILITY_API_KEY`) |
| **Headers** | `accept: application/json` (`multipart/form-data` request body) |

---

## 3. Implementation (`packages/ai/src/protocols/stability-images.ts`)

```typescript
/** `sd3.5-*` ids share the `sd3` endpoint and select the model with a form field. */
const endpoint = (model: string) => (model.startsWith("sd3") ? "sd3" : model)

const fromRequest = Effect.fn("StabilityImages.fromRequest")(function* (request: Request) {
  if (request.n !== undefined && request.n > 1)
    return yield* route.unsupported("media.n", `${route.name} generates one image per request; call it once per image`)
  const target = endpoint(request.model.id)
  const edit = (request.images?.length ?? 0) > 0
  if (edit && target === "core")
    return yield* route.unsupported("media.images", `${route.name} core is text-to-image only; use ultra or sd3.5-*`)
  return yield* form(
    route,
    {
      prompt: request.prompt,
      aspect_ratio: request.aspect,
      model: target === "sd3" ? request.model.id : undefined,
      mode: target === "sd3" && edit ? "image-to-image" : undefined,
    },
    mergeJsonRecords(request.providerOptions, request.http?.body),
    request.images,
  )
})
```
