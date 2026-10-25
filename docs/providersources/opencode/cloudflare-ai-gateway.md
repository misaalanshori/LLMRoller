# Cloudflare AI Gateway (`cloudflare-ai-gateway`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `cloudflare-ai-gateway`
- **Package Entrypoint**: `@opencode/ai/providers/cloudflare-ai-gateway` (`packages/ai/src/providers/cloudflare-ai-gateway.ts`)
- **Core Plugin**: `packages/core/src/plugin/provider/cloudflare-ai-gateway.ts` (`opencode.provider.cloudflare.ai.gateway`)
- **Protocols**:
  - `OpenAIChat.route` (`cloudflare-ai-gateway-chat`)
  - `OpenAIResponses.route` (`cloudflare-ai-gateway-responses`)
  - `AnthropicMessages.route` (`cloudflare-ai-gateway-messages`)
  - `CloudflareRunEvaluation.model` (`packages/ai/src/experimental/cloudflare-run-evaluation.ts`)

---

## 2. Endpoints, Passthrough Routing & Headers (`packages/ai/src/providers/cloudflare-ai-gateway.ts`)

| Route | Endpoint | Auth Header |
|---|---|---|
| **Default REST / Chat** | `https://api.cloudflare.com/client/v4/accounts/${accountId}/ai/v1/chat/completions` | `Authorization: Bearer <token>` + `cf-aig-gateway-id: <gatewayId>` |
| **OpenAI Passthrough (`openai/*`)** | `https://gateway.ai.cloudflare.com/v1/${accountId}/${gatewayId}/openai/responses` | `cf-aig-authorization: Bearer <token>` |
| **Anthropic Passthrough (`anthropic/*`)** | `https://gateway.ai.cloudflare.com/v1/${accountId}/${gatewayId}/anthropic/v1/messages` | `cf-aig-authorization: Bearer <token>` |
| **Experimental Run Evaluation** | `https://api.cloudflare.com/client/v4/accounts/${accountId}/ai/run/...` | `Authorization: Bearer <token>` |

- **Environment Variable Fallbacks**: `CLOUDFLARE_API_TOKEN`, `CF_AIG_TOKEN` (and `CLOUDFLARE_ACCOUNT_ID`, `CLOUDFLARE_GATEWAY_ID` in the Core plugin).
- **Gateway Option Headers**:
  - `cf-aig-gateway-id`: `input.gatewayId?.trim() || "default"`
  - `cf-aig-metadata`: `encodeJson(input.metadata)`
  - `cf-aig-cache-ttl`: `String(input.cacheTtl)`
  - `cf-aig-cache-key`: `input.cacheKey`
  - `cf-aig-skip-cache`: `String(input.skipCache)`
  - `cf-aig-collect-log`: `String(input.collectLog)`

---

## 3. Implementation (`packages/ai/src/providers/cloudflare-ai-gateway.ts`)

```typescript
const passthroughURL = (input: GatewayURL & GatewayOptions, provider: "anthropic/v1" | "openai") =>
  `https://gateway.ai.cloudflare.com/v1/${encodeURIComponent(requireAccountId(input))}/${encodeURIComponent(gatewayId(input))}/${provider}`

export const configure = (input: LanguageModelOptions) => {
  const custom = "auth" in input && input.auth ? input.auth : undefined
  const rest = {
    endpoint: { baseURL: baseURL(input) },
    auth: custom ?? credential(input).bearer(),
    headers: { "cf-aig-gateway-id": gatewayId(input), ...headers(input) },
    http: input.http,
    providerOptions: input.providerOptions,
  }
  const native = input.baseURL === undefined
  const passthrough = (provider: "anthropic/v1" | "openai") =>
    native
      ? {
          ...rest,
          endpoint: { baseURL: passthroughURL(input, provider) },
          auth: custom ?? Auth.bearerHeader("cf-aig-authorization", credential(input)),
          headers: headers(input),
        }
      : rest
  const responses = responsesRoute.with(passthrough("openai"))
  const messages = messagesRoute.with(passthrough("anthropic/v1"))
  const chat = route.with(rest)
  return {
    id,
    model: (value: string | ModelID) => {
      if (value.startsWith("openai/"))
        return responses.model<OpenAIProviderOptionsInput>({ id: native ? value.slice("openai/".length) : value })
      if (value.startsWith("anthropic/"))
        return messages.model<OpenAIProviderOptionsInput>({
          id: native ? value.slice("anthropic/".length).replaceAll(".", "-") : value,
        })
      if (value.startsWith("workers-ai/"))
        return chat.model<OpenAIProviderOptionsInput>({ id: value.slice("workers-ai/".length) })
      return chat.model<OpenAIProviderOptionsInput>({ id: value })
    },
    experimental: { evaluation },
    configure,
  }
}
```
