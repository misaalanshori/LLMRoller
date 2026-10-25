# GitLab Duo (`gitlab`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `gitlab`
- **Priority**: `100`
- **Hidden**: `true`
- **Display Name**: GitLab Duo
- **Category**: `oauth`
- **Website**: `https://gitlab.com`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source Files**:
  - `open-sse/providers/registry/gitlab.js`
  - `src/lib/oauth/providers/gitlab.js`

---

## 2. Endpoints & OAuth 2.0 PKCE Flow
- **Chat Completions Endpoint**: `POST https://gitlab.com/api/v4/chat/completions` (or custom instance `baseUrl` from `providerSpecificData.baseUrl`)
- **OAuth 2.0 Configuration (`authorization_code_pkce`, `S256`)**:
  - `defaultBaseUrl`: `https://gitlab.com`
  - `authorizeUrlPath`: `/oauth/authorize`
  - `tokenUrlPath`: `/oauth/token`
  - `userInfoUrlPath`: `/api/v4/user`
  - `scope`: `"api read_user"`
  - `codeChallengeMethod`: `"S256"`
- **Token Refresh (`DefaultExecutor.refreshGitLabToken`)**:
  - `POST <baseUrl>/oauth/token` (`application/x-www-form-urlencoded`) with `grant_type=refresh_token`, `refresh_token`, `client_id`, and optional `client_secret`.

---

## 3. Authentication & Headers
- **Authorization Header**: `Authorization: Bearer <accessToken_or_apiKey>` (`combined: true`)
- **Content-Type**: `application/json`

---

## 4. Verbatim Source Code Snippets

### `open-sse/providers/registry/gitlab.js`
```javascript
export default {
  id: "gitlab",
  hidden: true,
  priority: 100,
  display: {
    name: "GitLab Duo",
    icon: "code",
    color: "#FC6D26",
    textIcon: "GL",
    website: "https://gitlab.com",
    notice: {
      signupUrl: "https://gitlab.com",
    },
  },
  category: "oauth",
  transport: {
    baseUrl: "https://gitlab.com/api/v4/chat/completions",
    auth: {
      combined: true,
      header: "Authorization",
      scheme: "bearer",
    },
  },
  oauth: {
    defaultBaseUrl: "https://gitlab.com",
    authorizeUrlPath: "/oauth/authorize",
    tokenUrlPath: "/oauth/token",
    userInfoUrlPath: "/api/v4/user",
    scope: "api read_user",
    codeChallengeMethod: "S256",
  },
};
```

### `src/lib/oauth/providers/gitlab.js`
```javascript
import { GITLAB_CONFIG } from "../constants/oauth.js";

const gitlab = {
  config: GITLAB_CONFIG,
  flowType: "authorization_code_pkce",
  buildAuthUrl: (config, redirectUri, state, codeChallenge, meta = {}) => {
    const baseUrl = meta.baseUrl || config.defaultBaseUrl;
    const clientId = meta.clientId || "";
    const params = new URLSearchParams({
      client_id: clientId,
      redirect_uri: redirectUri,
      response_type: "code",
      state,
      scope: config.scope,
      code_challenge: codeChallenge,
      code_challenge_method: config.codeChallengeMethod,
    });
    return `${baseUrl}${config.authorizeUrlPath}?${params.toString()}`;
  },
  exchangeToken: async (config, code, redirectUri, codeVerifier, state, meta = {}) => {
    const baseUrl = meta.baseUrl || config.defaultBaseUrl;
    const clientId = meta.clientId || "";
    const clientSecret = meta.clientSecret || "";
    const body = new URLSearchParams({
      client_id: clientId,
      grant_type: "authorization_code",
      code,
      redirect_uri: redirectUri,
      code_verifier: codeVerifier,
    });
    if (clientSecret) body.set("client_secret", clientSecret);
    const response = await fetch(`${baseUrl}${config.tokenUrlPath}`, {
      method: "POST",
      headers: { "Content-Type": "application/x-www-form-urlencoded", Accept: "application/json" },
      body: body.toString(),
    });
    if (!response.ok) throw new Error(`GitLab token exchange failed: ${await response.text()}`);
    const tokens = await response.json();
    const userRes = await fetch(`${baseUrl}${config.userInfoUrlPath}`, {
      headers: { Authorization: `Bearer ${tokens.access_token}` },
    });
    const user = userRes.ok ? await userRes.json() : {};
    return { ...tokens, _user: user, _baseUrl: baseUrl, _clientId: clientId };
  },
```
