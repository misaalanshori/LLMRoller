# ElevenLabs (`elevenlabs`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `elevenlabs`
- **Package Entrypoint**: `packages/ai/src/providers/elevenlabs.ts`
- **Protocols**:
  - `packages/ai/src/protocols/elevenlabs-speech.ts` (`elevenlabs-speech`)
  - `packages/ai/src/protocols/elevenlabs-transcription.ts` (`elevenlabs-transcription`)

---

## 2. Endpoints & Authentication

| Service | Endpoint |
|---|---|
| **Default Base URL** | `https://api.elevenlabs.io` |
| **Speech (TTS)** | `POST /v1/text-to-speech/${voiceID}[/stream][/with-timestamps]?output_format={format}` |
| **Transcription (STT)** | `POST /v1/speech-to-text` (`multipart/form-data`) |
| **Authentication** | `xi-api-key: <apiKey>` (env fallback `ELEVENLABS_API_KEY`) |

---

## 3. Implementation (`packages/ai/src/providers/elevenlabs.ts`, `elevenlabs-speech.ts`, `elevenlabs-transcription.ts`)

```typescript
const auth = (options: ProviderAuthOption<"optional">) => {
  if ("auth" in options && options.auth) return options.auth
  return Auth.optional("apiKey" in options ? options.apiKey : undefined, "apiKey")
    .orElse(Auth.config("ELEVENLABS_API_KEY"))
    .pipe(Auth.header("xi-api-key"))
}

const path = (request: MediaProtocol.Addressed<Request>) =>
  `${PATH}/${encodeURIComponent(SpeechStream.voiceID(request.voice) ?? "")}${request.mode === "stream" ? "/stream" : ""}${
    request.timestamps === true ? "/with-timestamps" : ""
  }`
```

- **Speech (`elevenlabs-speech`)**: Supports 26 output formats (`mp3_44100_128`, `pcm_24000`, `wav_24000`, `opus_48000_64`, `ulaw_8000`, `alaw_8000`, etc.); extracts billed credit usage from the `character-cost` response header and character-level alignment (`character_start_times_seconds` / `character_end_times_seconds`) when `/with-timestamps` is active.
- **Transcription (`elevenlabs-transcription`)**: Sends `multipart/form-data` with `file` (or `source_url`), `model_id`, `language_code`, `diarize`, `num_speakers`; preserves `spacing` tokens when grouping speaker turns so unspaced languages are not re-spaced.
