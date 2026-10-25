# Zed (`zed`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `zed`
- **Display Name**: `Zed`
- **Alias**: `zd`
- **Category**: `oauth` (`rsaKeyExchange: true`)
- **Service Kinds**: `["llm"]`
- **Executor**: `ZedExecutor` (`open-sse/executors/zed.js`)
- **Auth & Catalog Helper**: `open-sse/shared/zedAuth.js`
- **Website**: `https://zed.dev`

---

## 2. Endpoints & RSA Keypair Authentication Protocol

| Step | Endpoint |
|---|---|
| **Native App Sign-In** | `GET https://zed.dev/native_app_signin?native_app_port={port}&native_app_public_key={b64url_pkcs1_der}&system_id={uuid}` |
| **User Info / Usage** | `GET https://cloud.zed.dev/client/users/me` (`Authorization: <user_id> <access_token>`) |
| **Mint Short-Lived LLM Token** | `POST https://cloud.zed.dev/client/llm_tokens` (`Authorization: <user_id> <access_token>`, `x-zed-system-id: <systemId>`) |
| **Live Model Catalog** | `GET https://cloud.zed.dev/models` (`Authorization: Bearer <llm_token>`, `x-zed-client-supports-x-ai: true`) |
| **Completions Stream** | `POST https://cloud.zed.dev/completions` (`Authorization: Bearer <llm_token>`) |

### Custom Zed Headers (`open-sse/shared/zedAuth.js`)
```javascript
export const ZED_HEADERS = {
  expiredToken: "x-zed-expired-token",
  outdatedToken: "x-zed-outdated-token",
  clientSupportsStatus: "x-zed-client-supports-status-messages",
  clientSupportsStreamEnded: "x-zed-client-supports-stream-ended-request-completion-status",
  serverSupportsStatus: "x-zed-server-supports-status-messages",
  clientSupportsXai: "x-zed-client-supports-x-ai",
  systemId: "x-zed-system-id",
};
```

---

## 3. Multi-Format Request & NDJSON Response Translation (`open-sse/executors/zed.js`)

Zed's `/completions` endpoint wraps the upstream provider's native request format inside `{ thread_id, prompt_id, provider, model, provider_request }` where `provider` is one of `"anthropic"`, `"open_ai"`, `"google"`, or `"x_ai"`:

```javascript
function buildProviderRequest(provider, model, body, stream, credentials) {
  if (provider === ZED_PROVIDER.anthropic) {
    return openaiToClaudeRequest(model, body, true);
  }
  if (provider === ZED_PROVIDER.google) {
    const geminiRequest = openaiToGeminiRequest(model, body, true);
    delete geminiRequest.safetySettings;
    return geminiRequest;
  }
  if (provider === ZED_PROVIDER.openai) {
    return openaiToOpenAIResponsesRequest(model, body, true, credentials);
  }
  // xAI is OpenAI-shaped — forward as-is.
  return { ...(body || {}), model, stream: stream !== false };
}

async execute({ model, body, stream, credentials, signal, log }) {
  const { provider } = await this.resolveModel(model, credentials, signal, log);
  const providerRequest = buildProviderRequest(provider, model, body, stream, credentials);
  const payload = {
    thread_id: body?.thread_id || credentials?._clientSessionId,
    prompt_id: body?.prompt_id,
    provider,
    model,
    provider_request: providerRequest,
  };

  const response = await zedLlmFetch(credentials, "/completions", {
    config: this.config,
    signal,
    fetchOptions: {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Accept: "application/x-ndjson, text/event-stream, */*",
        "User-Agent": "9router/zed",
        "x-zed-version": this.config?.appVersion?.toString() || "0.200.0",
        [ZED_HEADERS.clientSupportsStatus]: "true",
        [ZED_HEADERS.clientSupportsStreamEnded]: "true",
      },
      body: JSON.stringify(payload),
    },
  });
  const wrapped = response.ok ? wrapZedCompletionStream(response, provider, model) : response;
  return { response: wrapped, url: "https://cloud.zed.dev/completions", headers: {}, transformedBody: payload };
}
```
