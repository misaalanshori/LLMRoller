# Runway (`runway`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `runway`
- **Package Entrypoint**: `packages/ai/src/providers/runway.ts`
- **Protocol**: `packages/ai/src/protocols/runway-video.ts` (`runway-video`)

---

## 2. Endpoints, Headers & Authentication

| Operation | Endpoint |
|---|---|
| **Default Base URL** | `https://api.dev.runwayml.com/v1` |
| **Text-to-Video Start** | `POST /text_to_video` |
| **Image-to-Video Start (`frames.first` or `frames.last` set)** | `POST /image_to_video` |
| **Video-to-Video Start (`video` set)** | `POST /video_to_video` |
| **Poll Task Status & Result** | `GET /tasks/${taskID}` |
| **Cancel Task** | `DELETE /tasks/${taskID}` |
| **Authentication** | `Authorization: Bearer <secret>` (env fallback `RUNWAYML_API_SECRET`) |
| **Required Version Header** | `X-Runway-Version: 2024-11-06` |

---

## 3. Implementation (`packages/ai/src/protocols/runway-video.ts`)

```typescript
const fromRequest = Effect.fn("RunwayVideo.fromRequest")(function* (request: Request) {
  const first = request.frames?.first === undefined ? undefined : yield* mediaUri(request.frames.first)
  const last = request.frames?.last === undefined ? undefined : yield* mediaUri(request.frames.last)
  const promptImage = [
    ...(first === undefined ? [] : [{ uri: first, position: "first" }]),
    ...(last === undefined ? [] : [{ uri: last, position: "last" }]),
  ]
  const videoUri = request.video === undefined ? undefined : yield* mediaUri(request.video)
  const references = yield* Effect.forEach(request.references ?? [], (asset) =>
    mediaUri(asset).pipe(Effect.map((uri) => ({ uri }))),
  )
  return MediaProtocol.json(
    mergeJsonRecords(
      {
        model: request.model.id,
        promptText: request.prompt,
        promptImage: promptImage.length === 0 ? undefined : promptImage,
        videoUri,
        references: references.length === 0 ? undefined : references,
        ratio: request.aspect,
        duration: request.durationSeconds,
        resolution: request.resolution,
        audio: request.audio,
        negativePrompt: request.negativePrompt,
        seed: request.seed,
      },
      request.providerOptions,
      request.http?.body,
    ) ?? {},
  )
})
```
