# GitHub Copilot (`github`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `github`
- **Priority**: `40`
- **Alias / UI Alias**: `gh`
- **Display Name**: GitHub Copilot
- **Category**: `oauth`
- **Deprecated**: `true` (`deprecationNotice: "RISK_NOTICE"`)
- **Service Kinds**: `["llm", "embedding"]`
- **Website**: `https://github.com/features/copilot`
- **Executor**: `GithubExecutor` (`open-sse/executors/github.js`)
- **Source Files**:
  - `open-sse/providers/registry/github.js`
  - `open-sse/executors/github.js`
  - `src/lib/oauth/providers/github.js`

---

## 2. Tri-Endpoint Routing Architecture & Embeddings
`GithubExecutor` dynamically routes LLM requests across three GitHub Copilot endpoints depending on the model family:
1. **Chat Completions Endpoint (Default for GPT / Gemini / Kimi / MAI)**:
   - `POST https://api.githubcopilot.com/chat/completions`
   - Sanitizes `body.messages` via `sanitizeMessagesForChatCompletions` so only `text` and `image_url` content parts are sent (serializing `tool_use`, `tool_result`, `thinking` parts to `text`).
   - Converts `max_tokens` -> `max_completion_tokens` for `/gpt-5|o[134]-/i` models.
2. **Responses API Endpoint (Auto-Escalated for Codex / Responses-only Models)**:
   - `POST https://api.githubcopilot.com/responses`
   - If `/chat/completions` returns HTTP 400 containing `"not accessible via the /chat/completions endpoint"` or `"The requested model is not supported"` (and the model is not `gemini` or `claude`), adds the model to `this.knownCodexModels`, translates the payload via `openaiToOpenAIResponsesRequest`, and streams responses back via `openaiResponsesToOpenAIResponse`.
3. **Anthropic-Native Messages Endpoint (For all `claude` models, `/claude/i.test(model)`)**:
   - `POST https://api.githubcopilot.com/v1/messages`
   - Translates the OpenAI request to Claude format (`translateRequest(FORMATS.OPENAI, FORMATS.CLAUDE, ...)`), forces `stream: true`, and translates Claude SSE back to OpenAI chunks so prompt-cache token counts are preserved.
4. **Embeddings Endpoint**:
   - `POST https://api.githubcopilot.com/embeddings`
5. **Quota / User Info Endpoint**:
   - `GET https://api.github.com/copilot_internal/user`

---

## 3. Two-Stage OAuth Device Code & Copilot Token Exchange
1. **GitHub Device Code Flow (`src/lib/oauth/providers/github.js`)**:
   - `clientId`: `Iv1.b507a08c87ecfe98`
   - `deviceCodeUrl`: `POST https://github.com/login/device/code` (`scope: "read:user"`)
   - `tokenUrl`: `POST https://github.com/login/oauth/access_token` (`grant_type: "urn:ietf:params:oauth:grant-type:device_code"`)
2. **Short-Lived Copilot Session Token Exchange (`refreshCopilotToken`)**:
   - `GET https://api.github.com/copilot_internal/v2/token`
   - Headers: `Authorization: token <githubAccessToken>` (or `Bearer` during `postExchange`), `User-Agent: GitHubCopilotChat/0.38.0`, `Editor-Version: vscode/1.110.0`, `Editor-Plugin-Version: copilot-chat/0.38.0`, `x-github-api-version: 2025-04-01`.
   - Returns `{ token, expires_at }`. Refreshed automatically when `< 5 minutes` remain before `copilotTokenExpiresAt`.

---

## 4. Request Headers (`GithubExecutor.buildHeaders`)
```javascript
{
  "Authorization": `Bearer ${copilotToken || accessToken}`,
  "Content-Type": "application/json",
  "copilot-integration-id": "vscode-chat",
  "editor-version": "vscode/1.110.0",
  "editor-plugin-version": "copilot-chat/0.38.0",
  "user-agent": "GitHubCopilotChat/0.38.0",
  "openai-intent": "conversation-panel",
  "x-github-api-version": "2025-04-01",
  "x-request-id": "<uuid>",
  "x-vscode-user-agent-library-version": "electron-fetch",
  "X-Initiator": "user",
  "anthropic-version": "2023-06-01",
  "Accept": stream ? "text/event-stream" : "application/json"
}
```

---

## 5. Supported Models
| Model ID | Display Name | Kind | Target Endpoint |
|---|---|---|---|
| `gpt-5-mini`, `gpt-5.3-codex`, `gpt-5.4`, `gpt-5.4-mini`, `gpt-5.5` | GPT-5 Series | `llm` | `/chat/completions` or `/responses` |
| `gpt-5.6-luna`, `gpt-5.6-sol`, `gpt-5.6-terra` | GPT-5.6 Series | `llm` | `/chat/completions` or `/responses` |
| `gpt-6-astra`, `gpt-6-luna`, `gpt-6-sol`, `gpt-6.1-sol` | GPT-6 Series | `llm` | `/chat/completions` or `/responses` |
| `claude-fable-5`, `claude-fable-5.1`, `claude-haiku-4.5`, `claude-haiku-5.5`, `claude-opus-4.8`, `claude-opus-4.8-fast`, `claude-opus-5`, `claude-opus-5.5`, `claude-sonnet-4.6`, `claude-sonnet-5`, `claude-sonnet-5.5` | Claude Series | `llm` | `/v1/messages` |
| `gemini-3.7-flash`, `gemini-3.8-flash`, `gemini-4-argon` | Gemini Series | `llm` | `/chat/completions` |
| `kimi-k3` | Kimi K3 | `llm` | `/chat/completions` |
| `mai-code-1.1-flash` | MAI-Code-1.1-Flash | `llm` | `/chat/completions` |
| `text-embedding-3-small`, `text-embedding-ada-002` | Embeddings | `embedding` | `/embeddings` |

---

## 6. Verbatim Source Code Snippets

### `open-sse/executors/github.js` (Routing Logic Excerpt)
```javascript
  async execute(options) {
    const { model, log } = options;

    if (this.isClaudeModel(model)) {
      log?.debug("GITHUB", `Using /v1/messages route for ${model}`);
      return this.executeWithMessagesEndpoint(options);
    }

    if (this.knownCodexModels.has(model) && this.supportsResponsesEndpoint(model)) {
      log?.debug("GITHUB", `Using cached /responses route for ${model}`);
      return this.executeWithResponsesEndpoint(options);
    }

    const sanitizedOptions = {
      ...options,
      body: this.sanitizeMessagesForChatCompletions(options.body)
    };

    const result = await super.execute({ ...sanitizedOptions, proxyOptions: options.proxyOptions || null });

    if (result.response.status === HTTP_STATUS.BAD_REQUEST && this.supportsResponsesEndpoint(model)) {
      const errorBody = await result.response.clone().text();

      if (errorBody.includes("not accessible via the /chat/completions endpoint") || errorBody.includes("The requested model is not supported")) {
        log?.warn("GITHUB", `Model ${model} requires /responses. Switching...`);
        this.knownCodexModels.add(model);
        return this.executeWithResponsesEndpoint(options);
      }
    }

    return result;
  }
```
