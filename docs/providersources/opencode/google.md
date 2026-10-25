# Google Generative AI (`google`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `google`
- **Package Entrypoints**:
  - `@opencode/ai/providers/google` (`packages/ai/src/providers/google.ts`)
  - `@opencode/ai/providers/google/interactions` (`packages/ai/src/providers/google/interactions.ts`)
- **Protocols**:
  - `packages/ai/src/protocols/gemini.ts` (`gemini`)
  - `packages/ai/src/protocols/google-interactions.ts` (`google-interactions`)
  - `packages/ai/src/protocols/google-images.ts` (`google-images`)
  - `packages/ai/src/protocols/google-speech.ts` (`google-speech`)
  - `packages/ai/src/protocols/google-transcription.ts` (`google-transcription`)
  - `packages/ai/src/protocols/google-video.ts` (`google-video`)

---

## 2. Endpoints & Authentication (`packages/ai/src/providers/google.ts`)

| Service | Endpoint |
|---|---|
| **Default Base URL** | `https://generativelanguage.googleapis.com/v1beta` |
| **Gemini Chat (`gemini`)** | `POST /models/${model}:streamGenerateContent?alt=sse` |
| **Interactions API (`google-interactions`)** | `POST /interactions?alt=sse` |
| **Images (`google-images`)** | `POST /models/${model}:generateContent` (`responseModalities: ["IMAGE"]`) |
| **Speech (`google-speech`)** | `POST /models/${model}:{streamGenerateContent?alt=sse\|generateContent}` (`responseModalities: ["AUDIO"]`) |
| **Transcription (`google-transcription`)** | `POST /models/${model}:{streamGenerateContent?alt=sse\|generateContent}` (`audioTranscriptionConfig`) |
| **Veo Video (`google-video`)** | `POST /models/${model}:predictLongRunning` -> Poll `GET /${operation}` |
| **Authentication** | `x-goog-api-key: <apiKey>` (env fallback `GOOGLE_GENERATIVE_AI_API_KEY`) |

---

## 3. Provider & Protocol Implementation (`packages/ai/src/providers/google.ts`)

```typescript
const auth = (options: ProviderAuthOption<"optional">) => {
  if ("auth" in options && options.auth) return options.auth
  return Auth.optional("apiKey" in options ? options.apiKey : undefined, "apiKey")
    .orElse(Auth.config("GOOGLE_GENERATIVE_AI_API_KEY"))
    .pipe(Auth.header("x-goog-api-key"))
}

export const configure = (input: Config = {}) => {
  const route = configuredRoute(input)
  const media = MediaRoute.deployment(input, auth(input))
  return {
    id,
    model: (modelID: string | ModelID) => route.model<Gemini.ProviderOptionsInput>({ id: modelID }),
    interactions: (modelID: string | ModelID) =>
      interactionsRoute(input).model<GoogleInteractions.ProviderOptionsInput>({ id: modelID }),
    image: (modelID: string | ModelID) => GoogleImages.model({ ...media, id: modelID }),
    video: (modelID: string | ModelID) => GoogleVideo.model({ ...media, id: modelID }),
    speech: (modelID: string | ModelID) => GoogleSpeech.model({ ...media, id: modelID }),
    transcription: (modelID: string | ModelID) => GoogleTranscription.model({ ...media, id: modelID }),
    configure,
  }
}
```

### Key Protocol Specifics
- **Thought Signature Fallback (`gemini.ts`)**: For Gemini 3+ models (`requiresThoughtSignatureFallback`), unsigned replayed `functionCall` parts receive `thoughtSignature: "skip_thought_signature_validator"`.
- **Legacy Tool Media (`gemini.ts`)**: For `gemini-2.5*`, tool-result media attachments are hoisted into a separate `user` turn; Gemini 3+ embeds `parts` inside `functionResponse`.
- **Interactions API (`google-interactions.ts`)**: Stateful/stateless multi-step interaction protocol (`input: [{ type: "user_input" | "model_output" | "thought" | "function_call" | "function_result", ... }]`, `generation_config.thinking_level`, `previous_interaction_id`).
