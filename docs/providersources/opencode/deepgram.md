# Deepgram (`deepgram`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `deepgram`
- **Package Entrypoint**: `packages/ai/src/providers/deepgram.ts`
- **Protocols**:
  - `packages/ai/src/protocols/deepgram-speech.ts` (`deepgram-speech`)
  - `packages/ai/src/protocols/deepgram-transcription.ts` (`deepgram-transcription`)

---

## 2. Endpoints & Authentication

| Service | Endpoint |
|---|---|
| **Default Base URL** | `https://api.deepgram.com` |
| **Speech (TTS)** | `POST /v1/speak?model={model}&encoding={encoding}&container={container}&sample_rate={sampleRate}&bit_rate={bitRate}&speed={speed}` |
| **Transcription (STT)** | `POST /v1/listen?model={model}&smart_format=true&language={lang}&detect_language={bool}&diarize_model={latest}&utterances={bool}` |
| **Authentication** | `Authorization: Token <apiKey>` (`Auth.scheme("Token")`, env fallback `DEEPGRAM_API_KEY`) |

---

## 3. Implementation (`packages/ai/src/providers/deepgram.ts`, `deepgram-speech.ts`, `deepgram-transcription.ts`)

```typescript
const auth = (options: ProviderAuthOption<"optional">) => {
  if ("auth" in options && options.auth) return options.auth
  return Auth.optional("apiKey" in options ? options.apiKey : undefined, "apiKey")
    .orElse(Auth.config("DEEPGRAM_API_KEY"))
    .pipe(Auth.scheme("Token"))
}
```

- **Speech (`deepgram-speech`)**: Sends JSON `{ text: request.text }` with audio parameters in the query string; reads character usage from the `dg-char-count` response header and metadata from `dg-request-id` and `dg-model-name`.
- **Transcription (`deepgram-transcription`)**: Sends `{ url }` as JSON when given a URL or raw audio bytes (`Content-Type: <mediaType>`) when given inline audio; splits utterances at word-level speaker boundaries via `SpeakerTurns.group`.
