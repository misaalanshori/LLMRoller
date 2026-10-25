# GitLab Duo Agent Platform (`gitlab`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `gitlab`
- **Core Plugin**: `packages/core/src/plugin/provider/gitlab.ts` (`opencode.provider.gitlab`)
- **Underlying SDK**: `gitlab-ai-provider` (`createGitLab`, `discoverWorkflowModels`, `agenticChat`, `workflowChat`)

---

## 2. OAuth PKCE Flow, Token Refresh & Endpoints (`packages/core/src/plugin/provider/gitlab.ts`)

| Parameter | Value |
|---|---|
| **Default Instance URL** | `https://gitlab.com` (env override `GITLAB_INSTANCE_URL`) |
| **OAuth Authorize URL** | `${instanceUrl}/oauth/authorize` (`response_type=code`, `scope=api`, `code_challenge_method=S256`) |
| **OAuth Token / Refresh URL** | `POST ${instanceUrl}/oauth/token` (single-flight deduplicated refresh with 60s retention) |
| **Redirect URI** | `http://127.0.0.1:8080/callback` |
| **Bundled Client ID (`gitlab.com`)** | `fd180700a8f9c5d5557aca231632dd0611a1135a3bb510a741d5a988a3394fa7` (legacy refresh client ID: `1d89f9fdb23ee96d4e603201f6861dab6e143c5c3c00469a018a2d94bdc03d4e`, env override `GITLAB_OAUTH_CLIENT_ID`) |
| **Auth Headers** | OAuth: `Authorization: Bearer <access_token>`; PAT (`GITLAB_TOKEN`): `PRIVATE-TOKEN: <key>` |

---

## 3. SDK Configuration & Language Model Routing (`packages/core/src/plugin/provider/gitlab.ts`)

```typescript
yield* ctx.aisdk.hook(
  "sdk",
  Effect.fn(function* (evt) {
    if (evt.package !== "gitlab-ai-provider") return
    const mod = yield* Effect.promise(() => import("gitlab-ai-provider"))
    evt.sdk = mod.createGitLab({
      ...evt.options,
      instanceUrl:
        typeof evt.options.instanceUrl === "string"
          ? evt.options.instanceUrl
          : (process.env.GITLAB_INSTANCE_URL ?? "https://gitlab.com"),
      apiKey: typeof evt.options.apiKey === "string" ? evt.options.apiKey : process.env.GITLAB_TOKEN,
      aiGatewayHeaders: {
        "User-Agent": `${App.useragent(ctx.app)} gitlab-ai-provider/${mod.VERSION} (${os.platform()} ${os.release()}; ${os.arch()})`,
        "anthropic-beta": "context-1m-2025-08-07",
        ...evt.options.aiGatewayHeaders,
      },
      featureFlags: {
        duo_agent_platform_agentic_chat: true,
        duo_agent_platform: true,
        ...evt.options.featureFlags,
      },
    })
  }),
)
```
- Models starting with `duo-workflow-` use `evt.sdk.workflowChat(...)` with `selectedModelRef = workflowRef`; other models use `evt.sdk.agenticChat(id, ...)`.
