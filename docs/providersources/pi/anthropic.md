# Anthropic (`anthropic`) Provider Specification (`pi`)

## 1. Overview & Identification

- **Provider ID**: `anthropic`
- **Display Name**: `Anthropic`
- **Provider Module**: `packages/ai/src/providers/anthropic.ts`
- **API Implementation**: `packages/ai/src/api/anthropic-messages.ts` (`anthropic-messages`)
- **OAuth Implementation**: `packages/ai/src/auth/oauth/anthropic.ts`
- **Models Catalog**: `packages/ai/src/providers/anthropic.models.ts` (`ANTHROPIC_MODELS`)

---

## 2. Endpoints, Authentication & Workload Identity Federation

| Parameter | Value |
|---|---|
| **Base URL** | `https://api.anthropic.com` |
| **Messages Endpoint** | `POST /v1/messages?beta=true` (via `@anthropic-ai/sdk` `client.beta.messages.stream`) |
| **OAuth Authorize URL** | `https://claude.ai/oauth/authorize` |
| **OAuth Token URL** | `POST https://platform.claude.com/v1/oauth/token` |
| **OAuth Client ID** | `9d1c250a-e61b-44d9-88ed-5944d1962f5e` (base64-encoded in source) |
| **OAuth Callback / Redirect URI** | `http://localhost:53692/callback` (or `https://platform.claude.com/oauth/code/callback` for copy-code mode) |
| **OAuth Scopes** | `org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload` |

### API Key & Federation Precedence (`packages/ai/src/providers/anthropic.ts`)
1. Stored `credential.key` (`x-api-key`)
2. `ANTHROPIC_AUTH_TOKEN` -> `Authorization: Bearer <authToken>`
3. `ANTHROPIC_OAUTH_TOKEN` or `ANTHROPIC_API_KEY`
4. **Workload Identity Federation**: `ANTHROPIC_FEDERATION_RULE_ID`, `ANTHROPIC_ORGANIZATION_ID`, `ANTHROPIC_IDENTITY_TOKEN_FILE`, plus optional `ANTHROPIC_SERVICE_ACCOUNT_ID` and `ANTHROPIC_WORKSPACE_ID`.

---

## 3. Claude Code Stealth Mode (`packages/ai/src/api/anthropic-messages.ts`)

When authenticating with an OAuth token (`sk-ant-oat*`), `anthropic-messages.ts`:
- Emulates Claude Code `2.1.280` headers (`User-Agent: claude-cli/2.1.280 (external, cli)`, `x-app: cli`, `anthropic-dangerous-direct-browser-access: true`) and beta flags (`claude-code-20250219`, `oauth-2025-04-20`, `interleaved-thinking-2025-05-14`).
- Normalizes tool names case-insensitively to canonical Claude Code 2.x tool casing (`Read`, `Write`, `Edit`, `Bash`, `Grep`, `Glob`, `AskUserQuestion`, `EnterPlanMode`, `ExitPlanMode`, `KillShell`, `NotebookEdit`, `Skill`, `Task`, `TaskOutput`, `TodoWrite`, `WebFetch`, `WebSearch`) on outbound requests and maps them back on inbound tool calls.
