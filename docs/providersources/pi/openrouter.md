# OpenRouter (`openrouter`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `openrouter`
- **API Protocols**:
  - Chat: `anthropic-messages` | `openai-completions`
  - Image Generation: `openrouter-images` (`packages/ai/src/api/openrouter-images.ts`)
  - Classifier: `typesafe-system-one`
- **Base URL**: `https://openrouter.ai/api/v1`
- **Source Files**:
  - `packages/ai/src/providers/openrouter.ts`
  - `packages/ai/src/providers/openrouter-api.ts`
  - `packages/ai/src/auth/oauth/openrouter.ts`

## 2. Authentication & Catalog Personalization
- **Environment Variable**: `OPENROUTER_API_KEY`
- **OAuth**: Supports PKCE OAuth (`loadOpenRouterOAuth` -> `https://openrouter.ai/auth` and `POST https://openrouter.ai/api/v1/auth/keys`).
- **Dynamic Catalog Personalization (`personalizeOpenRouterCatalog`)**:
  - Queries `GET https://openrouter.ai/api/v1/models/user` with `Authorization: Bearer <key>` (15s timeout).
  - Filters chat models to those allowed for the user's key/region and updates `contextWindow`, `maxTokens`, and `cost` from `top_provider` and `pricing`.

## 3. Protocol Specifics
- **Anthropic Cache Control**: When routing `anthropic/*` models via `openai-completions`, injects `cache_control: { type: "ephemeral" }` on system/user/assistant text blocks (`maybeAddOpenRouterAnthropicCacheControl`).
- **Provider Routing**: Forwards `model.compat.openRouterRouting` in the request payload under `provider`.

## 4. Source Implementation (`packages/ai/src/providers/openrouter.ts`)
```typescript
export function openrouterProvider(): Provider<"anthropic-messages" | "openai-completions"> {
	return createProvider<"anthropic-messages" | "openai-completions">({
		id: "openrouter",
		name: "OpenRouter",
		baseUrl: OPENROUTER_BASE_URL,
		auth: {
			apiKey: envApiKeyAuth("OpenRouter API key", ["OPENROUTER_API_KEY"]),
			oauth: lazyOAuth({
				name: "OpenRouter OAuth",
				loginLabel: "Sign in with OpenRouter",
				load: loadOpenRouterOAuth,
			}),
		},
		models: [
			...Object.values(OPENROUTER_MODELS),
			...Object.values(OPENROUTER_IMAGE_MODELS),
			...Object.values(OPENROUTER_CLASSIFIER_MODELS),
		],
		refreshModels: personalizeOpenRouterCatalog,
		api: {
			"anthropic-messages": anthropicMessagesApi(),
			"openai-completions": openAICompletionsApi(),
		},
		images: {
			"openrouter-images": openrouterImagesApi(),
		},
		classifiers: {
			"typesafe-system-one": typesafeSystemOneApi(),
		},
	});
}
```
