# OpenAI (`openai`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `openai`
- **API Protocols**:
  - Chat / Reasoning: `openai-responses` (`POST /v1/responses`)
  - Classifier: `openai-decisions` (`packages/ai/src/api/openai-decisions.ts`)
- **Base URL**: `https://api.openai.com/v1`
- **Source Files**:
  - `packages/ai/src/providers/openai.ts`
  - `packages/ai/src/providers/openai.models.ts`
  - `packages/ai/src/api/openai-responses.ts`
  - `packages/ai/src/api/openai-responses-shared.ts`
  - `packages/ai/src/auth/oauth/openai-chatgpt.ts`

## 2. Authentication (API Key & ChatGPT Subscription OAuth)
1. **API Key**: `OPENAI_API_KEY` (`sk-...`).
2. **Sign in with ChatGPT OAuth (`loadOpenAIChatGPTOAuth`)**:
   - Tokens that do not start with `sk-` sent to `https://api.openai.com/v1` are recognized via `isChatGPTSignIn()`.
   - Filters out `openai-decisions` classifier models when authenticated via ChatGPT OAuth (`filterAllModels`), as the Decisions API rejects ChatGPT OAuth tokens.

## 3. Protocol & Request Formatting (`openai-responses`)
- **Minimum Output Tokens**: Clamps `max_output_tokens` to at least `16` (`OPENAI_RESPONSES_MIN_OUTPUT_TOKENS = 16`).
- **Prompt Caching**: Injects `prompt_cache_key` (clamped via `clampOpenAIPromptCacheKey`) and `prompt_cache_retention: "24h"` when `PI_CACHE_RETENTION=long`.
- **Reasoning**: Configures `reasoning: { effort, summary: "auto" }` and `include: ["reasoning.encrypted_content"]`.

## 4. Source Implementation (`packages/ai/src/providers/openai.ts`)
```typescript
export function openaiProvider(): Provider<"openai-responses"> {
	return createProvider<"openai-responses">({
		id: "openai",
		name: "OpenAI",
		baseUrl: "https://api.openai.com/v1",
		auth: {
			apiKey: envApiKeyAuth("OpenAI API key", ["OPENAI_API_KEY"]),
			oauth: lazyOAuth({
				name: "OpenAI (ChatGPT subscription)",
				isSubscription: true,
				loginLabel: "Sign in with ChatGPT",
				load: loadOpenAIChatGPTOAuth,
			}),
		},
		models: [
			...Object.values(OPENAI_MODELS),
			...Object.values<ClassifierModel<"openai-decisions">>(OPENAI_CLASSIFIER_MODELS),
		],
		// Sign in with ChatGPT tokens only reach the Responses API; the Decisions API rejects them.
		filterAllModels: (models, credential) =>
			credential?.type === "oauth" ? models.filter((model) => !isModelType(model, "classifier")) : models,
		api: openAIResponsesApi(),
		classifiers: {
			"openai-decisions": openAIDecisionsApi(),
		},
	});
}
```
