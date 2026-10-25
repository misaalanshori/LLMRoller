# OpenCode Console (`opencode`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `opencode`
- **Package Entrypoint**: `@opencode/ai/providers/opencode` (`packages/ai/src/providers/opencode.ts`)
- **Core Plugin**: `packages/core/src/plugin/provider/opencode.ts` (`opencode.provider.opencode`)

---

## 2. Endpoints & Authentication

| Service | Endpoint |
|---|---|
| **Default Console Server** | `https://opencode.ai/console` (rewrites legacy `https://console.opencode.ai`) |
| **System One Evaluation Base URL** | `https://opencode.ai/inference/systemone/v1` (`POST /systemone`) |
| **Device Code Request** | `POST ${server}/auth/device/code` (`{ client_id: "opencode-cli", supports_org_scope: true }`) |
| **Device Token Polling / Refresh** | `POST ${server}/auth/device/token` |
| **User Profile** | `GET ${server}/api/user` |
| **User Organizations** | `GET ${server}/api/orgs` |
| **Remote Provider / Policy / MCP Config** | `GET ${server}/api/v2/config` (polled every 1 minute while connected) |
| **Console Web Search** | `POST ${server}/api/websearch` (`{ query, providerID }`) |
| **SSO Start URL** | `${server}/auth/sso/${connectionID}/start?redirectTo=${redirectTo}` |

---

## 3. Headers, Public Fallback & Organization Policies (`packages/core/src/plugin/provider/opencode.ts`)

- **Authenticated Headers (`credentialHeaders`)**:
  - `authorization: Bearer <access_token | api_key>`
  - `x-org-id: <orgID>` (when `metadata.orgID` is set)
  - `User-Agent: <App.useragent(ctx.app)>`
- **Unauthenticated Free-Model Fallback**:
  - When no `OPENCODE_API_KEY` or Console connection is configured and `OPENCODE_DISABLE_FREE_MODELS` is not `"1"`/`"true"`, sets `provider.activation = "enabled"`, `provider.settings.apiKey = "public"`, and disables paid models (`cost.input > 0`).
- **Managed Policies & Remote Providers**:
  - Applies `config.providers`, `config.mcp.servers` (attaching Console credentials when `auth === "console"`), `config.websearch`, and `config.policies` from `GET ${server}/api/v2/config`.
