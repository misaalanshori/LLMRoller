# OpenAI (`openai`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `openai`
- **Package Entrypoints**:
  - `@opencode/ai/providers/openai` (`packages/ai/src/providers/openai.ts`)
  - `@opencode/ai/providers/openai/chat` (`packages/ai/src/providers/openai/chat.ts`)
  - `@opencode/ai/providers/openai/responses` (`packages/ai/src/providers/openai/responses.ts`)
  - `packages/ai/src/providers/openai-options.ts`
- **Protocols**:
  - `packages/ai/src/protocols/openai-responses.ts` (`openai-responses`)
  - `packages/ai/src/protocols/openai-chat.ts` (`openai-chat`)
  - `packages/ai/src/protocols/openai-images.ts` (`openai-images`)
  - `packages/ai/src/protocols/openai-speech.ts` (`openai-speech`)
  - `packages/ai/src/protocols/openai-transcription.ts` (`openai-transcription`)
  - `packages/ai/src/experimental/openai-decisions.ts` (`openai-decisions`)
- **Core Plugins**:
  - `packages/core/src/plugin/provider/openai.ts` (`opencode.provider.openai`)
  - `packages/core/src/plugin/provider/openai-context-caps.ts` (`opencode.provider.openai-context-caps`)

---

## 2. Endpoints, Transports & Authentication

| Service | Endpoint |
|---|---|
| **Default Base URL** | `https://api.openai.com/v1` |
| **Codex OAuth Backend Base URL** | `https://chatgpt.com/backend-api/codex` (when authenticated via `chatgpt-browser` or `chatgpt-headless`) |
| **Responses API (`openai-responses`)** | `POST /responses` (supports HTTP SSE and WebSocket channel transport; native compaction at `POST /responses/compact`) |
| **Chat Completions (`openai-chat`)** | `POST /chat/completions` |
| **Images (`openai-images`)** | `POST /images/generations` or `POST /images/edits` |
| **Speech (`openai-speech`)** | `POST /audio/speech` |
| **Transcription (`openai-transcription`)** | `POST /audio/transcriptions` |
| **Authentication** | `Authorization: Bearer <apiKey>` (env fallback `OPENAI_API_KEY`); optional headers `OpenAI-Organization` and `OpenAI-Project` |

---

## 3. Legacy Codex OAuth & WebSocket / Zstd Configuration (`packages/core/src/plugin/provider/openai.ts`)

- **OAuth Client ID**: `app_EMoamEEZ73f0CkXaXp7hrann` (`issuer = "https://auth.openai.com"`)
- **Browser PKCE (`chatgpt-browser`)**: `https://auth.openai.com/oauth/authorize` (`redirect_uri: "http://localhost:1455/auth/callback"`, `codex_cli_simplified_flow: "true"`, `id_token_add_organizations: "true"`, `originator: "opencode"`).
- **Headless Device Code (`chatgpt-headless`)**: `POST https://auth.openai.com/api/accounts/deviceauth/usercode` -> user visits `https://auth.openai.com/codex/device` -> polls `POST https://auth.openai.com/api/accounts/deviceauth/token` -> exchanges at `POST https://auth.openai.com/oauth/token`.
- **Codex Backend Headers**:
  - `originator: opencode`
  - `x-codex-beta-features: remote_compaction_v2`
  - `chatgpt-account-id: <accountID>` (extracted from JWT `chatgpt_account_id` or `organizations[0].id`)
  - `session-id: <affinity>`
- **Transport & Compression**: Sets `transport: "websocket"` by default and `encoding: "zstd"` for ChatGPT Codex connections; strips `maxTokens` on `context` and `compaction` requests.
- **Context Caps (`openai-context-caps.ts`)**: Caps ChatGPT-signed-in input window to `272,000` tokens for models prior to `gpt-6.1` and `500,000` tokens for `gpt-6.1+`.
