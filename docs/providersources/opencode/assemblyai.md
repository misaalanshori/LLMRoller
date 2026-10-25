# AssemblyAI (`assemblyai`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `assemblyai`
- **Package Entrypoint**: `packages/ai/src/providers/assemblyai.ts`
- **Protocol**: `packages/ai/src/protocols/assemblyai-transcription.ts`
- **Capabilities**: Queued audio transcription (`TranscriptionModel`)

---

## 2. Endpoints & Authentication

| Operation | Endpoint |
|---|---|
| **Base URL** | `https://api.assemblyai.com` (or `https://api.eu.assemblyai.com` for EU) |
| **Inline Audio Upload** | `POST /v2/upload` (`Content-Type: application/octet-stream`) -> `{ upload_url }` |
| **Submit Transcription Job** | `POST /v2/transcript` |
| **Poll Status & Result** | `GET /v2/transcript/{transcriptID}` |
| **Authentication** | Raw API key in `authorization: <apiKey>` header (no `Bearer` prefix); env fallback `ASSEMBLYAI_API_KEY` |

---

## 3. Provider & Queued Transcription Implementation

### 3.1 Provider Configuration (`packages/ai/src/providers/assemblyai.ts`)

```typescript
// The key is the whole `authorization` value, without a scheme.
const auth = (options: ProviderAuthOption<"optional">) => {
  if ("auth" in options && options.auth) return options.auth
  return Auth.optional("apiKey" in options ? options.apiKey : undefined, "apiKey")
    .orElse(Auth.config("ASSEMBLYAI_API_KEY"))
    .pipe(Auth.header("authorization"))
}

export const configure = (input: Config = {}) => {
  const media = MediaRoute.deployment(input, auth(input))
  const transcription = (modelID: string | ModelID) => AssemblyAITranscription.model({ ...media, id: modelID })
  return {
    id,
    transcription,
    configure,
  }
}
```

### 3.2 Two-Step Upload & Queued Transcript Polling (`packages/ai/src/protocols/assemblyai-transcription.ts`)

```typescript
/** `/v2/transcript` only takes a URL, so inline audio is uploaded to `/v2/upload` first. */
const prepare = Effect.fn("AssemblyAITranscription.prepare")(function* (request: Request, send: MediaProtocol.Send) {
  if (request.audio.source.type !== "bytes" && request.audio.source.type !== "base64") return request
  const audio = yield* MediaInput.inlineBytes(route.id, request.audio)
  const uploaded = yield* send(UPLOAD_PATH, MediaProtocol.binary(audio, "application/octet-stream")).pipe(
    Effect.flatMap(decodeUpload),
  )
  return { ...request, audio: Media.url(uploaded.value.upload_url, { mediaType: request.audio.mediaType }) }
})

const fromRequest = Effect.fn("AssemblyAITranscription.fromRequest")(function* (request: Request) {
  const audio = yield* ProviderShared.mediaReference(request.audio, route.provider, route.name)
  return MediaProtocol.json(
    mergeJsonRecords(
      {
        audio_url: audio.value,
        speech_models: [request.model.id],
        language_code: request.language,
        language_detection: request.language === undefined ? true : undefined,
        prompt: request.prompt,
        speaker_labels:
          request.diarize === true || request.timestamps === "segment" || request.speakers !== undefined
            ? true
            : undefined,
        speakers_expected: request.speakers,
      },
      request.providerOptions,
      request.http?.body,
    ) ?? {},
  )
})
```
- Maps `utterances` (milliseconds -> seconds) to `segments` and `words` (milliseconds -> seconds) to `words`, with `usage: { type: "seconds", seconds: duration }`.
