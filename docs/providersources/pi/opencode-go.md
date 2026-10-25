# OpenCode Go (`opencode-go`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `opencode-go`
- **API Protocols**: `anthropic-messages`, `google-generative-ai`, `openai-completions`, `openai-responses`
- **Base URL**: `https://opencode.ai/zen/go/v1` (per model in `opencode-go.models.ts`)
- **Source Files**:
  - `packages/ai/src/providers/opencode-go.ts`
  - `packages/ai/src/providers/opencode-go.models.ts`
  - `packages/ai/src/providers/opencode-headers.ts`

## 2. Authentication & Session Headers
- **Environment Variable**: `OPENCODE_API_KEY`
- **Session Header Injection**: Uses `withOpenCodeSessionHeader` across all 4 protocol adapters (`anthropic-messages`, `google-generative-ai`, `openai-completions`, `openai-responses`).
- **Models**: Hosts `glm-5`, `glm-5.1`, `kimi-k2.5`, `minimax-m2.5`, `minimax-m2.7`, `qwen3.5-plus`, `qwen3.6-plus`.

## 3. Source Implementation (`packages/ai/src/providers/opencode-go.ts`)
```typescript
type OpenCodeGoApi = "anthropic-messages" | "google-generative-ai" | "openai-completions" | "openai-responses";

export function opencodeGoProvider(): Provider<OpenCodeGoApi> {
	return createProvider<OpenCodeGoApi>({
		id: "opencode-go",
		name: "OpenCode Go",
		auth: { apiKey: envApiKeyAuth("OpenCode API key", ["OPENCODE_API_KEY"]) },
		models: Object.values(OPENCODE_GO_MODELS),
		api: {
			"anthropic-messages": withOpenCodeSessionHeader(anthropicMessagesApi()),
			"google-generative-ai": withOpenCodeSessionHeader(googleGenerativeAIApi()),
			"openai-completions": withOpenCodeSessionHeader(openAICompletionsApi()),
			"openai-responses": withOpenCodeSessionHeader(openAIResponsesApi()),
		},
	});
}
```
