# ChatGPT Token Sharing (`chatgpt`) Provider Plugin Specification (`opencode`)

## 1. Overview & Identification

- **Plugin ID**: `opencode.provider.chatgpt` (`packages/core/src/plugin/provider/chatgpt.ts`)
- **Target Provider ID**: `openai` (`methodID: "chatgpt-token-sharing"`)
- **Purpose**: Dynamic OAuth 2.0 + PKCE + OIDC client registration (`dynamic_agent_client`) with ChatGPT subscription token sharing (`chatgpt.tokens.use.direct`) against `https://api.openai.com/v1`.

---

## 2. OAuth & API Endpoints (`packages/core/src/plugin/provider/chatgpt.ts`)

| Endpoint | Value |
|---|---|
| **Issuer** | `https://auth.openai.com` |
| **Authorization URL** | `https://auth.openai.com/api/accounts/authorize` |
| **Token Exchange / Refresh URL** | `POST https://auth.openai.com/api/accounts/oauth/token` |
| **JWKS Verification URL** | `https://auth.openai.com/.well-known/jwks.json` (RS256 `id_token` verification) |
| **Resource / API Base URL** | `https://api.openai.com/v1` |
| **Initial Registration Client ID** | `dynamic_agent_client` (`agent_name_hint: "OpenCode"`, `ext_agent_host_id: "urn:uuid:<uuid>"`) |
| **Required Scope** | `openid profile email offline_access resource.invoke chatgpt.tokens.use.direct` |

---

## 3. Request Headers, Zstd Compression & Fallback Models

- **Session Affinity Headers (`model.request` hook)**:
  - `session-id`: `SessionAffinity.get(session)` (or `evt.sessionID`)
  - `thread-id`: `evt.sessionID`
  - `x-client-request-id`: `evt.sessionID`
- **Transport & Compression**:
  - Forces `transport: "http"`, `compaction: { type: "summary" }`, and `encoding: "zstd"` (because ChatGPT sign-in rejects uncompressed bodies over ~1 MiB and decodes zstd).
- **Fallback Models**:
  `gpt-5.5`, `gpt-5.5-fast`, `gpt-5.6-luna`, `gpt-5.6-luna-fast`, `gpt-5.6-sol`, `gpt-5.6-sol-fast`, `gpt-5.6-terra`, `gpt-5.6-terra-fast`, `gpt-6-astra`, `gpt-6-astra-fast`, `gpt-6-luna`, `gpt-6-luna-fast`, `gpt-6-sol`, `gpt-6-sol-fast`, `gpt-6.1-sol`, `gpt-6.1-sol-fast`.

```typescript
function authorizeURL(
  redirect: string,
  pkce: Pkce,
  state: string,
  nonce: string,
  savedID: string | undefined,
  hostID: string,
) {
  return `${issuer}/api/accounts/authorize?${new URLSearchParams({
    client_id: savedID ?? registrationClientID,
    ...(savedID ? {} : { agent_name_hint: agentName }),
    ext_agent_host_id: hostID,
    response_type: "code",
    redirect_uri: redirect,
    scope: `openid profile email offline_access resource.invoke ${tokenSharingScope}`,
    resource,
    state,
    nonce,
    code_challenge_method: "S256",
    code_challenge: pkce.challenge,
  })}`
}
```
