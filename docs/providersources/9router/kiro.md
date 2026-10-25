# Kiro AI (`kiro`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `kiro`
- **Priority**: `10`
- **Alias / UI Alias**: `kr`
- **Display Name**: Kiro AI (AWS CodeWhisperer / Amazon Q)
- **Category**: `free`
- **Deprecated**: `true` (`deprecationNotice: "RISK_NOTICE"`)
- **Website**: `https://kiro.dev`
- **Executor**: `KiroExecutor` (`open-sse/executors/kiro.js`)
- **Source Files**:
  - `open-sse/providers/registry/kiro.js`
  - `open-sse/executors/kiro.js`
  - `open-sse/config/kiroConstants.js`
  - `src/lib/oauth/providers/kiro.js`

---

## 2. Multi-Surface Regional Endpoints & Binary EventStream Transport
- **Configured Base URLs (`getOrderedBaseUrls` orders Amazon `q.*` -> `codewhisperer.*` -> `runtime.*.kiro.dev` with regionalization)**:
  1. `https://q.<region>.amazonaws.com/generateAssistantResponse` (default `us-east-1`)
  2. `https://codewhisperer.<region>.amazonaws.com/generateAssistantResponse` (adds header `X-Amz-Target: AmazonCodeWhispererStreamingService.GenerateAssistantResponse`)
  3. `https://runtime.us-east-1.kiro.dev/generateAssistantResponse`
- **Wire Format**: `"kiro"` (Request is CodeWhisperer `conversationState` JSON; Response is binary `application/vnd.amazon.eventstream` with CRC32 prelude verification, decoded into OpenAI SSE chunks by `KiroExecutor.transformEventStreamToSSE` and guarded by `attachIntegrityGate` / `runIntegrityRecovery`).
- **Usage Limits Endpoints**:
  - `https://codewhisperer.us-east-1.amazonaws.com/getUsageLimits`
  - `https://q.us-east-1.amazonaws.com/getUsageLimits`

---

## 3. Authentication Modes, Headers & AWS SSO OIDC / Social Auth
- **Supported Auth Methods**: `["builder-id", "idc", "google", "github", "import", "api_key", "external_idp"]`
- **AWS SSO OIDC Dynamic Client Registration + Device Flow (`src/lib/oauth/providers/kiro.js`)**:
  1. `POST https://oidc.<region>.amazonaws.com/client/register` with `{ clientName: "kiro-oauth-client", clientType: "public", scopes: ["codewhisperer:completions", "codewhisperer:analysis", "codewhisperer:conversations"], grantTypes: ["urn:ietf:params:oauth:grant-type:device_code", "refresh_token"], issuerUrl: "https://identitycenter.amazonaws.com/ssoins-722374e8c3c8e6c6" }`.
  2. `POST https://oidc.<region>.amazonaws.com/device_authorization` with `{ clientId, clientSecret, startUrl }`.
  3. `POST https://oidc.<region>.amazonaws.com/token`.
- **Social Auth Endpoints (Google / GitHub via Kiro Desktop Auth)**:
  - `socialLoginUrl`: `https://prod.us-east-1.auth.desktop.kiro.dev/login`
  - `socialTokenUrl`: `https://prod.us-east-1.auth.desktop.kiro.dev/oauth/token`
  - `socialRefreshUrl`: `https://prod.us-east-1.auth.desktop.kiro.dev/refreshToken`
- **Request Headers (`KiroExecutor.buildHeaders`)**:
  - `Content-Type`: `"application/json"`
  - `Accept`: `"application/vnd.amazon.eventstream"`
  - `User-Agent`: `"AWS-SDK-JS/3.0.0 kiro-ide/1.0.0"`
  - `X-Amz-User-Agent`: `"aws-sdk-js/3.0.0 kiro-ide/1.0.0"`
  - `Amz-Sdk-Request`: `"attempt=1; max=3"`
  - `Amz-Sdk-Invocation-Id`: `<uuidv4>`
  - `Authorization`: `Bearer <accessToken_or_apiKey>`
  - `TokenType`: `"API_KEY"` (when `authMethod === "api_key"`) or `"EXTERNAL_IDP"` (when `authMethod === "external_idp"`)
  - `x-amz-sso-bearer`: `<accessToken>`
  - `x-amzn-kiro-agent-mode`: `"spec"`
  - `x-amzn-codewhisperer-machine-id`: `"kiro-desktop"`
  - `x-amzn-codewhisperer-profile-arn`: `<profileArn>` (defaults: Builder ID `arn:aws:codewhisperer:us-east-1:638616132270:profile/AAAACCCCXXXX`, Social `arn:aws:codewhisperer:us-east-1:699475941385:profile/EHGA3GRVQMUK`)

---

## 4. EventStream Frame Types & Integrity Gate (`KiroExecutor`)
- **Decoded EventStream `:event-type` Frames**:
  - `assistantResponseEvent` (`payload.content`, with `<thinking>...</thinking>` stripping)
  - `reasoningContentEvent` (`payload.reasoningContentEvent.text`)
  - `codeEvent` (`payload.content`)
  - `toolUseEvent` (`payload.name`, `payload.toolUseId`, `payload.input`)
  - `messageStopEvent` / `metadataEvent` (`stopReason`)
  - `contextUsageEvent` (`contextUsagePercentage`)
  - `meteringEvent` (`kiro_credits`) & `metricsEvent` (`inputTokens`, `outputTokens`, `cacheReadInputTokens`)
- **Integrity Gate & Auto-Repair (`runIntegrityRecovery`)**:
  - Buffers up to 8 MiB (`KIRO_REPAIR_BUFFER_MAX_BYTES`) while emitting `: kiro-validation\n\n` heartbeats every 10s.
  - Automatically retries once with a repair instruction appended to `currentMessage.userInputMessage.content` if Kiro produces an ellipsis-only reply (`...`), a short future-action announcement without results, or a malformed `tool_call` wrapper.
- **Synthetic Suffixes (`kiroConstants.js`)**:
  - `-thinking`: Injects `<thinking_mode>enabled</thinking_mode>\n<max_thinking_length>16000</max_thinking_length>` or native `additionalModelRequestFields`.
  - `-agentic`: Injects `KIRO_AGENTIC_SYSTEM_PROMPT` (mandatory 300-line chunked write protocol).

---

## 5. Supported Models
- **Claude Series** (each available in base, `-thinking`, `-agentic`, and `-thinking-agentic` variants):
  - `claude-opus-5.5`, `claude-opus-5`, `claude-opus-4.8`, `claude-opus-4.7`, `claude-opus-4.5`
  - `claude-sonnet-5`, `claude-sonnet-4.5`
  - `claude-haiku-4.5`
- **OpenAI GPT-5.6 Series** (272k context, base, `-thinking`, `-agentic`, `-thinking-agentic`):
  - `gpt-5.6-sol` (`rateMultiplier: 2.4`), `gpt-5.6-terra` (`rateMultiplier: 1.2`), `gpt-5.6-luna` (`rateMultiplier: 0.6`)
- **Additional Models**:
  - `deepseek-3.2` (`strip: ["image", "audio"]`), `qwen3-coder-next` (`strip: ["image", "audio"]`), `glm-5`, `MiniMax-M2.5`

---

## 6. Verbatim Source Code Snippets

### `open-sse/executors/kiro.js` (`buildHeaders` & `getOrderedBaseUrls`)
```javascript
  buildHeaders(credentials, stream = true, url = "") {
    const headers = {
      ...this.config.headers,
      "Amz-Sdk-Request": "attempt=1; max=3",
      "Amz-Sdk-Invocation-Id": uuidv4()
    };
    if (url.includes("://codewhisperer.")) {
      headers["X-Amz-Target"] = KIRO_CODEWHISPERER_TARGET;
    } else {
      delete headers["X-Amz-Target"];
    }

    const authMethod = credentials?.providerSpecificData?.authMethod;
    const isApiKey = authMethod === "api_key";
    const isExternalIdp = authMethod === "external_idp";

    const apiKey = credentials?.apiKey || (isApiKey ? credentials?.accessToken : null);
    if (isApiKey && apiKey) {
      headers["Authorization"] = `Bearer ${apiKey}`;
      headers["TokenType"] = "API_KEY";
    } else if (credentials?.accessToken) {
      headers["Authorization"] = `Bearer ${credentials.accessToken}`;
      if (isExternalIdp) {
        headers["TokenType"] = "EXTERNAL_IDP";
      }
    }

    if (credentials?.accessToken) {
      headers["x-amz-sso-bearer"] = credentials.accessToken;
    }
    headers["x-amzn-kiro-agent-mode"] = "spec";
    headers["x-amzn-codewhisperer-machine-id"] = "kiro-desktop";
    const profileArn = credentials?.providerSpecificData?.profileArn;
    if (profileArn) {
      headers["x-amzn-codewhisperer-profile-arn"] = profileArn;
    }

    return headers;
  }
```
