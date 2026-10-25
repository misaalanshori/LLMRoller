# Meta AI (`meta`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `meta`
- **Package Entrypoints**:
  - `@opencode/ai/providers/meta` (`packages/ai/src/providers/meta.ts`)
  - `@opencode/ai/providers/meta/chat` (`packages/ai/src/providers/meta/chat.ts`)
  - `@opencode/ai/providers/meta/messages` (`packages/ai/src/providers/meta/messages.ts`)
  - `@opencode/ai/providers/meta/responses` (`packages/ai/src/providers/meta/responses.ts`)
- **Protocols**:
  - `packages/ai/src/protocols/meta-responses.ts` (`meta-responses`)
  - `packages/ai/src/protocols/meta-messages.ts` (`meta-messages`)
  - `packages/ai/src/protocols/meta-images.ts` (`meta-images`)
  - `OpenAIChat.protocol` (`meta-chat`)

---

## 2. Endpoints & Authentication (`packages/ai/src/providers/meta.ts`)

| Route | Base URL | Path | Protocol & Defaults |
|---|---|---|---|
| **Responses (`meta-responses`)** | `https://api.meta.ai/v1` | `POST /responses` | `MetaResponses.protocol` (HTTP/SSE only, `store: false`, `include: ["reasoning.encrypted_content"]`) |
| **Chat (`meta-chat`)** | `https://api.meta.ai/v1` | `POST /chat/completions` | `OpenAIChat.protocol` (`maxTokensField: "max_completion_tokens"`, `supportsPromptCacheKey: true`) |
| **Messages (`meta-messages`)** | `https://api.meta.ai/v1` | `POST /messages` | `MetaMessages.protocol` (`thinking: { type: "adaptive", display: "omitted" }`, `requireSignature: false`) |
| **Images (`meta-images`)** | `https://api.meta.ai/v1` | `POST /images/generations` or `POST /images/edits` (when `images` present) | `MetaImages.protocol` |

- **Authentication**: `Authorization: Bearer <apiKey>` (env fallback `META_API_KEY`).

---

## 3. Hosted Tools & Protocol Quirks (`meta-responses.ts` & `meta-images.ts`)

- **Hosted Tools**:
  - `webSearch({ searchContextSize?, userLocation? })` -> `native: { meta: { type: "web_search", search_context_size, user_location } }`
  - `imageGeneration({ size?, outputFormat?, reasoningStrength?, enableImageSearch?, enableWebSearch?, enableShell? })` -> `native: { meta: { type: "image_generation", ... } }` (Muse Image).
- **Muse Image Replay Handle (`meta-responses.ts`)**: Replays `{ type: "image_generation_call", id: part.id, status: "completed", result: null }` so Meta's signed image ID preserves multi-turn edit state without re-uploading raw image bytes.
- **Terminal Snapshot Recovery (`meta-responses.ts`)**: Muse Image delivers its generated image only in `response.completed` (while Spark streams items and changes reasoning IDs in the terminal snapshot); `MetaResponses.step` deduplicates by `completedItems` and `output_index`.
