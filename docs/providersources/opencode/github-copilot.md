# GitHub Copilot (`github-copilot`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `github-copilot`
- **Core Plugin**: `packages/core/src/plugin/provider/github-copilot.ts` (`opencode.provider.github.copilot`)
- **Bundled SDK & Models**:
  - `packages/core/src/github-copilot/copilot-provider.ts`
  - `packages/core/src/github-copilot/models.ts`
  - `packages/core/src/github-copilot/chat/*`
  - `packages/core/src/github-copilot/responses/*`

---

## 2. OAuth Device Flow, Entitlement & API Endpoints

| Operation | Endpoint |
|---|---|
| **Device Code Request** | `POST https://${domain}/login/device/code` (`client_id: "Ov23li8tweQw6odWQebz"`, `scope: "read:user"`) |
| **Device Token Polling** | `POST https://${domain}/login/oauth/access_token` (`grant_type: "urn:ietf:params:oauth:grant-type:device_code"`) |
| **Copilot Entitlement & API Discovery** | `GET https://api.${domain}/copilot_internal/user` (`X-GitHub-Api-Version: 2025-04-01`) -> checks `chat_enabled` and reads `endpoints.api` |
| **Default Copilot Base URL** | `https://api.githubcopilot.com` (or `https://copilot-api.${enterpriseDomain}`) |
| **Live Models Catalog** | `GET ${baseURL}/models` (`X-GitHub-Api-Version: 2026-08-01`) |
| **Chat Completions** | `POST ${baseURL}/chat/completions` |
| **OpenAI Responses** | `POST ${baseURL}/responses` |
| **Anthropic Messages** | `POST ${baseURL}/v1/messages` |

---

## 3. Headers & Multi-Protocol Routing (`packages/core/src/plugin/provider/github-copilot.ts`)

```typescript
function applyHeaders(
  headers: Headers,
  token: string | undefined,
  app: App.Info,
  metadata: RequestMetadata,
  anthropic: boolean,
) {
  if (token) {
    headers.delete("authorization")
    headers.delete("x-api-key")
    headers.set("Authorization", `Bearer ${token}`)
  }
  headers.set("User-Agent", App.useragent(app))
  headers.set("Openai-Intent", "conversation-edits")
  headers.set("X-GitHub-Api-Version", apiVersion) // "2026-08-01"
  if (metadata.agent) headers.set("x-initiator", "agent")
  else if (!headers.has("x-initiator")) headers.set("x-initiator", "user")
  if (metadata.vision) headers.set("Copilot-Vision-Request", "true")
  if (anthropic) headers.set("anthropic-beta", "interleaved-thinking-2025-05-14")
}
```

- **Interaction Headers (`model.request` hook)**:
  - `X-Interaction-Type`: `"conversation-agent"` (default), `"conversation-subagent"` (child session), `"conversation-background"` (`kind === "title"`), `"conversation-compaction"` (`kind === "compaction"`), or `"agent-session-name-generation"` (utility title via `gpt-4o-mini`).
  - `X-Interaction-Id`: `SessionAffinity.get(session)`
  - `x-initiator`: `"agent"` (when tool results/assistant turns are replayed or non-agent interaction) or `"user"`.
- **Protocol Selection**:
  - Claude models use `@opencode/ai/providers/anthropic` against `${baseURL}/v1/messages` (with `x-api-key` rewritten to `Authorization: Bearer <token>` in `http.request`).
  - GPT-5+ (except `gpt-5-mini`), `grok-*`, and `mai-code-*` use `/responses`.
  - Gemini and GPT-4o / `gpt-5-mini` use `/chat/completions`.
