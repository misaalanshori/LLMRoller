# Black Forest Labs (`black-forest-labs`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `black-forest-labs`
- **Package Entrypoint**: `packages/ai/src/providers/black-forest-labs.ts`
- **Protocol**: `packages/ai/src/protocols/bfl-images.ts` (`bfl-images`)
- **Capabilities**: Queued image generation (`ImageModel`)

---

## 2. Endpoints & Authentication

| Operation | Endpoint |
|---|---|
| **Default Base URL** | `https://api.bfl.ai` (regional overrides: `https://api.eu.bfl.ai`, `https://api.us.bfl.ai`) |
| **Start Generation** | `POST /v1/${request.model.id}` -> returns `{ id, polling_url, cost? }` |
| **Poll Status & Result** | `GET ${token.pollingURL}` (regional cluster URL returned by submit) |
| **Authentication** | `x-key: <apiKey>` (env fallback `BFL_API_KEY`) |

---

## 3. Model Capabilities & Queued Polling (`packages/ai/src/protocols/bfl-images.ts`)

```typescript
const capabilities = (model: string): Capabilities => {
  if (model.startsWith("flux-pro-1.0-fill")) return { sizing: "none", imageField: "image", maxImages: 1, mask: true }
  if (model.startsWith("flux-pro-1.0-expand")) return { sizing: "none", imageField: "image", maxImages: 1, mask: false }
  if (model.startsWith("flux-kontext"))
    return { sizing: "aspectRatio", imageField: "input_image", maxImages: 4, mask: false }
  if (model.startsWith("flux-pro-1.1-ultra"))
    return { sizing: "aspectRatio", imageField: "image_prompt", maxImages: 1, mask: false }
  if (model.startsWith("flux-pro-1.1") || model.startsWith("flux-dev"))
    return { sizing: LEGACY_LIMITS, imageField: "image_prompt", maxImages: 1, mask: false }
  if (model.startsWith("flux-2-klein"))
    return { sizing: FLUX_2_LIMITS, imageField: "input_image", maxImages: 4, mask: false }
  return { sizing: FLUX_2_LIMITS, imageField: "input_image", maxImages: 8, mask: false }
}
```
- Maps multiple reference images to `input_image`, `input_image_2`, `input_image_3`, etc.
- Downloads `result.sample` immediately upon `Ready` because BFL signed URLs expire after 10 minutes.
