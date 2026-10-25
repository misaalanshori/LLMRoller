# Cartesia (`cartesia`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `cartesia`
- **Package Entrypoint**: `packages/ai/src/providers/cartesia.ts`
- **Protocol**: `packages/ai/src/protocols/cartesia-speech.ts` (`cartesia-speech`)
- **Capabilities**: Streaming and non-streaming speech synthesis (`SpeechModel`)

---

## 2. Endpoints, Headers & Authentication

| Parameter | Value |
|---|---|
| **Default Base URL** | `https://api.cartesia.ai` |
| **Bytes Path (Non-streaming without timestamps)** | `POST /tts/bytes` |
| **SSE Path (Streaming or `timestamps: true`)** | `POST /tts/sse` |
| **Authentication** | `Authorization: Bearer <apiKey>` (env fallback `CARTESIA_API_KEY`) |
| **Static Version Header** | `Cartesia-Version: 2026-08-14` |

---

## 3. Request & Stream Implementation (`packages/ai/src/protocols/cartesia-speech.ts`)

```typescript
const fromRequest = Effect.fn("CartesiaSpeech.fromRequest")(function* (request: MediaProtocol.Addressed<Request>) {
  const voice = SpeechStream.voiceID(request.voice)
  if (voice === undefined)
    return yield* ProviderShared.invalidRequest(`${route.name} requires a voice id; pass it as \`voice\``)
  const { sampleRate: _sampleRate, bitRate: _bitRate, encoding: _encoding, ...native } = request.providerOptions ?? {}
  return MediaProtocol.json(
    mergeJsonRecords(
      {
        model_id: request.model.id,
        transcript: request.text,
        voice,
        output_format: yield* outputFormat(request),
        language: request.language,
        generation_config: request.speed === undefined ? undefined : { speed: request.speed },
        add_timestamps: request.timestamps === true ? true : undefined,
      },
      native,
      request.http?.body,
    ) ?? {},
  )
})
```
- Supports `pcm` (`container: "raw"`, `encoding: "pcm_s16le"`), `wav`, and `mp3` on `/tts/bytes`, and raw PCM with base64 `chunk` + `timestamps` (`word_timestamps`) + `done` events on `/tts/sse`.
