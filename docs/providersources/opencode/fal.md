# fal (`fal`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `fal`
- **Package Entrypoint**: `packages/ai/src/providers/fal.ts`
- **Protocols & Queue Helper**:
  - `packages/ai/src/protocols/fal-images.ts` (`fal-images`)
  - `packages/ai/src/protocols/fal-video.ts` (`fal-video`)
  - `packages/ai/src/protocols/utils/fal-queue.ts` (`FalQueue`)

---

## 2. Endpoints & Authentication

| Operation | Endpoint |
|---|---|
| **Default Base URL** | `https://queue.fal.run` |
| **Submit Queued Job** | `POST https://queue.fal.run/${request.model.id}` -> returns `{ request_id, status_url, response_url, cancel_url, queue_position? }` |
| **Poll Status** | `GET ${token.statusURL}` (`IN_QUEUE` / `IN_PROGRESS` / `COMPLETED`) |
| **Fetch Result** | `GET ${token.responseURL}` |
| **Cancel Job** | `PUT ${token.cancelURL}` |
| **Authentication** | `Authorization: Key <apiKey>` (`Auth.scheme("Key")`, env fallback `FAL_KEY`) |

---

## 3. Implementation (`packages/ai/src/providers/fal.ts` & `fal-queue.ts`)

```typescript
// fal authenticates with `Authorization: Key <FAL_KEY>` rather than a bearer token.
const auth = (options: ProviderAuthOption<"optional">) => {
  if ("auth" in options && options.auth) return options.auth
  return Auth.optional("apiKey" in options ? options.apiKey : undefined, "apiKey")
    .orElse(Auth.config("FAL_KEY"))
    .pipe(Auth.scheme("Key"))
}

export const configure = (input: Config = {}) => {
  const media = MediaRoute.deployment(input, auth(input))
  return {
    id,
    image: (modelID: string | ModelID) => FalImages.model({ ...media, id: modelID }),
    video: (modelID: string | ModelID) => FalVideo.model({ ...media, id: modelID }),
    configure,
  }
}
```
- **Image Sizing (`fal-images.ts`)**: `fal-ai/nano-banana*` uses `aspect_ratio` + `resolution` tiers; `fal-ai/flux-pro/(v1.1-ultra|kontext)` uses `aspect_ratio`; `fal-ai/flux*` uses `image_size: { width, height }`. `/edit` and `/multi` models take `image_urls: string[]` whereas single-image models take `image_url` + optional `mask_url`.
- **Video (`fal-video.ts`)**: Sends `{ prompt, negative_prompt, seed, aspect_ratio, resolution, generate_audio, image_url, video_url, ...providerOptions }`.
