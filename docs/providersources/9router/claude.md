# Claude Code (`claude`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `claude`
- **Priority**: `10`
- **Alias / UI Alias**: `cc`
- **Display Name**: Claude Code
- **Category**: `oauth`
- **Deprecated**: `true` (`deprecationNotice: "RISK_NOTICE"`)
- **Website**: `https://claude.ai`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source Files**:
  - `open-sse/providers/registry/claude.js`
  - `src/lib/oauth/providers/claude.js`
  - `open-sse/utils/claudeCloaking.js`
  - `open-sse/translator/index.js`

---

## 2. Endpoints & Transport
- **Messages Endpoint**: `POST https://api.anthropic.com/v1/messages?beta=true` (`baseUrl` + `urlSuffix: "?beta=true"`)
- **Wire Format**: `"claude"`
- **Usage & Profile Endpoints**:
  - `oauthUrl`: `https://api.anthropic.com/api/oauth/usage`
  - `orgUrl`: `https://api.anthropic.com/v1/organizations/{org_id}/usage`
  - `settingsUrl`: `https://api.anthropic.com/v1/settings`
  - `profileUrl`: `https://api.anthropic.com/api/oauth/profile`
  - `resetUrl`: `https://api.anthropic.com/api/organizations/{org_id}/reset_rate_limits`

---

## 3. Authentication, Headers & OAuth 2.0 PKCE Flow
- **Split Auth Configuration**:
  - When `credentials.apiKey` is present: `x-api-key: <apiKey>` (`scheme: "raw"`)
  - When `credentials.accessToken` is present: `Authorization: Bearer <accessToken>` (`scheme: "bearer"`)
- **Static Claude CLI Impersonation Headers**:
  - `Anthropic-Version`: `2023-06-01`
  - `Anthropic-Beta`: `claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27,prompt-caching-scope-2026-01-05,advanced-tool-use-2025-11-20,effort-2025-11-24,structured-outputs-2025-12-15,fast-mode-2026-02-01,redact-thinking-2026-02-12,token-efficient-tools-2026-03-28`
  - `Anthropic-Dangerous-Direct-Browser-Access`: `true`
  - `User-Agent`: `claude-cli/2.1.114 (external, sdk-cli)` (`CLAUDE_CLI_VERSION`)
  - `X-App`: `cli`
  - `X-Stainless-Helper-Method`: `stream`
  - `X-Stainless-Retry-Count`: `0`
  - `X-Stainless-Runtime-Version`: `v24.14.0`
  - `X-Stainless-Package-Version`: `0.80.0`
  - `X-Stainless-Runtime`: `node`
  - `X-Stainless-Lang`: `js`
  - `X-Stainless-Arch`: `arm64`
  - `X-Stainless-Os`: `MacOS`
  - `X-Stainless-Timeout`: `600`
  - Plus per-session `X-Claude-Code-Session-Id: <uuid>` injected by `DefaultExecutor.buildHeaders` when no `x-claude-code-session-id` header is present.
- **OAuth 2.0 Authorization Code + PKCE (`S256`)**:
  - `clientId`: `9d1c250a-e61b-44d9-88ed-5944d1962f5e`
  - `authorizeUrl`: `https://claude.ai/oauth/authorize` (passes `code=true`)
  - `tokenUrl`: `https://api.anthropic.com/v1/oauth/token` (JSON body `{ code, state, grant_type: "authorization_code", client_id, redirect_uri, code_verifier }`; parses `code#state` split if `#` is present in pasted authorization code)
  - `scopes`: `["org:create_api_key", "user:profile", "user:inference"]`
  - `refreshLeadMs`: `14400000` (4 hours)
  - `refresh.encoding`: `"json"`

---

## 4. Anti-Ban Cloaking & Tool Obfuscation (`cloakToolsOnOAuth: true`)
When authenticating with a Claude OAuth token (`apiKey.includes("sk-ant-oat")`), `9router` applies two layers of cloaking in `open-sse/utils/claudeCloaking.js`:
1. **Billing Header & Fake User ID (`applyCloaking`)**:
   - Prepends `{ type: "text", text: "x-anthropic-billing-header: cc_version=2.1.114.<3hex>; cc_entrypoint=sdk-cli; cch=<5hex_sha256_of_body>;" }` as `system[0]`.
   - Sets `metadata.user_id` to `{"device_id":"<64hex>","account_uuid":"<uuid>","session_id":"<uuid>"}` deterministically derived from the token and aligned with `X-Claude-Code-Session-Id`.
2. **Tool Name Cloaking (`cloakClaudeTools` / `decloakToolNames` / `decloakStreamChunk`)**:
   - Renames all client-defined tools (excluding built-in server tools with a `type` field) by appending `CLAUDE_TOOL_SUFFIX` (`"_ide"`) in `tools[]`, historical `tool_use` blocks in `messages[]`, and forced `tool_choice`.
   - Appends 20 `CC_DECOY_TOOLS` (`Task`, `TaskOutput`, `TaskStop`, `TaskCreate`, `TaskGet`, `TaskUpdate`, `TaskList`, `Bash`, `Glob`, `Grep`, `Read`, `Edit`, `Write`, `NotebookEdit`, `WebFetch`, `WebSearch`, `AskUserQuestion`, `Skill`, `EnterPlanMode`, `ExitPlanMode`) marked `"This tool is currently unavailable."`.
   - Restores the original tool names on incoming responses (`decloakToolNames` for non-streaming JSON, `decloakStreamChunk` on `content_block_start` SSE events).

---

## 5. Supported Models
| Model ID | Display Name |
|---|---|
| `claude-opus-5-5` | Claude Opus 5.5 |
| `claude-opus-5` | Claude Opus 5 |
| `claude-fable-5-1` | Claude Fable 5.1 |
| `claude-fable-5` | Claude Fable 5 |
| `claude-sonnet-5-5` | Claude Sonnet 5.5 |
| `claude-sonnet-5` | Claude Sonnet 5 |
| `claude-haiku-4-5-20251001` | Claude 4.5 Haiku |

---

## 6. Verbatim Source Code Snippets

### `open-sse/providers/registry/claude.js`
```javascript
import { CLAUDE_CLI_VERSION } from "../shared.js";

export default {
  id: "claude",
  priority: 10,
  alias: "cc",
  uiAlias: "cc",
  display: {
    name: "Claude Code",
    icon: "smart_toy",
    color: "#D97757",
    website: "https://claude.ai",
    notice: {
      signupUrl: "https://claude.ai",
    },
    deprecated: true,
    deprecationNotice: "RISK_NOTICE",
  },
  category: "oauth",
  transport: {
    baseUrl: "https://api.anthropic.com/v1/messages",
    format: "claude",
    urlSuffix: "?beta=true",
    headers: {
      "Anthropic-Version": "2023-06-01",
      "Anthropic-Beta": "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27,prompt-caching-scope-2026-01-05,advanced-tool-use-2025-11-20,effort-2025-11-24,structured-outputs-2025-12-15,fast-mode-2026-02-01,redact-thinking-2026-02-12,token-efficient-tools-2026-03-28",
      "Anthropic-Dangerous-Direct-Browser-Access": "true",
      "User-Agent": `claude-cli/${CLAUDE_CLI_VERSION} (external, sdk-cli)`,
      "X-App": "cli",
      "X-Stainless-Helper-Method": "stream",
      "X-Stainless-Retry-Count": "0",
      "X-Stainless-Runtime-Version": "v24.14.0",
      "X-Stainless-Package-Version": "0.80.0",
      "X-Stainless-Runtime": "node",
      "X-Stainless-Lang": "js",
      "X-Stainless-Arch": "arm64",
      "X-Stainless-Os": "MacOS",
      "X-Stainless-Timeout": "600",
    },
    quirks: {
      cloakToolsOnOAuth: true,
    },
    auth: {
      apiKey: {
        header: "x-api-key",
        scheme: "raw",
      },
      oauth: {
        header: "Authorization",
        scheme: "bearer",
      },
    },
    usage: {
      oauthUrl: "https://api.anthropic.com/api/oauth/usage",
      orgUrl: "https://api.anthropic.com/v1/organizations/{org_id}/usage",
      settingsUrl: "https://api.anthropic.com/v1/settings",
      profileUrl: "https://api.anthropic.com/api/oauth/profile",
      resetUrl: "https://api.anthropic.com/api/organizations/{org_id}/reset_rate_limits",
    },
  },
  models: [
    { id: "claude-opus-5-5", name: "Claude Opus 5.5" },
    { id: "claude-opus-5", name: "Claude Opus 5" },
    { id: "claude-fable-5-1", name: "Claude Fable 5.1" },
    { id: "claude-fable-5", name: "Claude Fable 5" },
    { id: "claude-sonnet-5-5", name: "Claude Sonnet 5.5" },
    { id: "claude-sonnet-5", name: "Claude Sonnet 5" },
    { id: "claude-haiku-4-5-20251001", name: "Claude 4.5 Haiku" },
  ],
  oauth: {
    clientId: "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
    authorizeUrl: "https://claude.ai/oauth/authorize",
    tokenUrl: "https://api.anthropic.com/v1/oauth/token",
    scopes: [
      "org:create_api_key",
      "user:profile",
      "user:inference",
    ],
    codeChallengeMethod: "S256",
    refreshLeadMs: 14400000,
    refresh: {
      encoding: "json",
    },
  },
  features: {
    usage: true,
  },
};
```

### `open-sse/utils/claudeCloaking.js` (Excerpt)
```javascript
function generateBillingHeader(payload) {
  const content = JSON.stringify(payload);
  const cch = createHash("sha256").update(content).digest("hex").slice(0, 5);
  const buildHash = randomBytes(2).toString("hex").slice(0, 3);
  return `x-anthropic-billing-header: cc_version=${CLAUDE_CLI_VERSION}.${buildHash}; cc_entrypoint=${CC_ENTRYPOINT}; cch=${cch};`;
}

export function applyCloaking(body, apiKey, sessionId) {
  if (!apiKey || !apiKey.includes("sk-ant-oat")) return body;
  const result = { ...body };
  const billingText = generateBillingHeader(body);
  const billingBlock = { type: "text", text: billingText };

  if (Array.isArray(result.system)) {
    if (!result.system[0]?.text?.startsWith("x-anthropic-billing-header:")) {
      result.system = [billingBlock, ...result.system];
    }
  } else if (typeof result.system === "string") {
    result.system = [billingBlock, { type: "text", text: result.system }];
  } else {
    result.system = [billingBlock];
  }

  const existingUserId = result.metadata?.user_id;
  if (!existingUserId) {
    result.metadata = { ...result.metadata, user_id: generateFakeUserID(sessionId, apiKey) };
  }
  return result;
}
```
