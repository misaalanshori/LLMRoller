# OpenCode Zen (`opencode`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `opencode`
- **API Protocols**:
  - `anthropic-messages`
  - `google-generative-ai`
  - `openai-completions`
  - `openai-responses`
  - Classifier: `typesafe-system-one` (`/zen/v1/systemone`)
- **Base URL**: `https://opencode.ai/zen/v1` (per model in `opencode.models.ts`)
- **Source Files**:
  - `packages/ai/src/providers/opencode.ts`
  - `packages/ai/src/providers/opencode.models.ts`
  - `packages/ai/src/providers/opencode-headers.ts`

## 2. Authentication & Session Headers
- **Environment Variable**: `OPENCODE_API_KEY`
- **Session Affinity Wrapper (`withOpenCodeSessionHeader`)**: Wraps all 4 underlying API adapters (`anthropic-messages`, `google-generative-ai`, `openai-completions`, `openai-responses`) to inject OpenCode session/request headers when `options.sessionId` is present.

## 3. Source Implementation (`packages/ai/src/providers/opencode.ts`)
```typescript
type OpenCodeApi = "anthropic-messages" | "google-generative-ai" | "openai-completions" | "openai-responses";

export function opencodeProvider(): Provider<OpenCodeApi> {
	return createProvider<OpenCodeApi>({
		id: "opencode",
		name: "OpenCode Zen",
		auth: { apiKey: envApiKeyAuth("OpenCode API key", ["OPENCODE_API_KEY"]) },
		models: [...Object.values(OPENCODE_MODELS), ...Object.values(OPENCODE_CLASSIFIER_MODELS)],
		api: {
			"anthropic-messages": withOpenCodeSessionHeader(anthropicMessagesApi()),
			"google-generative-ai": withOpenCodeSessionHeader(googleGenerativeAIApi()),
			"openai-completions": withOpenCodeSessionHeader(openAICompletionsApi()),
			"openai-responses": withOpenCodeSessionHeader(openAIResponsesApi()),
		},
		// OpenCode Zen serves TypeSafe's System One protocol at /zen/v1/systemone.
		classifiers: { "typesafe-system-one": typesafeSystemOneApi() },
	});
}
```
