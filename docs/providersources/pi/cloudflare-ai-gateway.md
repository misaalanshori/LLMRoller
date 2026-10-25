# Cloudflare AI Gateway (`cloudflare-ai-gateway`) Provider Specification (`pi`)

## 1. Overview & Identification

- **Provider ID**: `cloudflare-ai-gateway`
- **Display Name**: `Cloudflare AI Gateway`
- **Provider Modules**:
  - `packages/ai/src/providers/cloudflare-ai-gateway.ts`
  - `packages/ai/src/providers/cloudflare-auth.ts`
  - `packages/ai/src/providers/cloudflare-stream.ts`
- **API & Binding Modules**:
  - `packages/ai/src/api/cloudflare.ts`
  - `packages/ai/src/api/cloudflare-ai-binding.ts`
- **Models Catalog**: `packages/ai/src/providers/cloudflare-ai-gateway.models.ts` (`CLOUDFLARE_AI_GATEWAY_MODELS`)

---

## 2. Endpoints, Headers & Workers AI Binding (`packages/ai/src/api/cloudflare.ts` & `cloudflare-auth.ts`)

| Endpoint Constant | URL Template |
|---|---|
| **Unified Compat API** | `https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/compat` |
| **OpenAI Passthrough** | `https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/openai` |
| **Anthropic Passthrough** | `https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/anthropic` |
| **Workers AI Binding Route** | `https://workers-binding.ai/ai-gateway/gateways/{gateway}/{provider}/...` (`createAiBindingFetch(env.AI)`) |

- **Authentication (`cloudflareAIGatewayAuth`)**:
  Requires `CLOUDFLARE_API_KEY`, `CLOUDFLARE_ACCOUNT_ID`, and `CLOUDFLARE_GATEWAY_ID`, and sets:
  ```typescript
  headers: {
    "cf-aig-authorization": `Bearer ${resolved.apiKey}`,
    Authorization: null,
    "x-api-key": null,
  }
  ```
  (Setting `Authorization: null` and `x-api-key: null` strips placeholder SDK auth headers so the gateway uses its stored BYOK keys rather than treating a placeholder as a provider key).

---

## 3. Implementation (`packages/ai/src/providers/cloudflare-ai-gateway.ts`)

```typescript
export function cloudflareAIGatewayProvider(): Provider<CloudflareAIGatewayApi> {
	return createProvider<CloudflareAIGatewayApi>({
		id: "cloudflare-ai-gateway",
		name: "Cloudflare AI Gateway",
		auth: { apiKey: cloudflareAIGatewayAuth() },
		models: Object.values(CLOUDFLARE_AI_GATEWAY_MODELS),
		api: {
			"anthropic-messages": cloudflareStreams(anthropicMessagesApi()),
			"openai-completions": cloudflareStreams(openAICompletionsApi()),
			"openai-responses": cloudflareStreams(openAIResponsesApi()),
		},
	});
}
```
