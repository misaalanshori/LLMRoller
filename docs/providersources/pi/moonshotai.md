# Moonshot AI (`moonshotai`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `moonshotai`
- **API Protocol**: `openai-completions` (`POST /v1/chat/completions`)
- **Base URL**: `https://api.moonshot.ai/v1`
- **Source Files**:
  - `packages/ai/src/providers/moonshotai.ts`
  - `packages/ai/src/api/openai-completions.ts`
  - `packages/ai/src/env-api-keys.ts`

## 2. Authentication & Compatibility
- **Environment Variable**: `MOONSHOT_API_KEY`
- **Authorization Header**: `Authorization: Bearer <MOONSHOT_API_KEY>`
- **Compatibility Flags**:
  - `supportsDeveloperRole: false`
  - `thinkingFormat: "zai"` on `kimi-k2.5` and `kimi-k2-thinking*` (`thinking: { type: "enabled" | "disabled" }`).

## 3. Source Implementation (`packages/ai/src/providers/moonshotai.ts`)
```typescript
const MOONSHOT_BASE_URL = "https://api.moonshot.ai/v1";

const MOONSHOT_COMPAT = {
	supportsDeveloperRole: false,
} as const;

const MOONSHOT_THINKING_COMPAT = {
	supportsDeveloperRole: false,
	thinkingFormat: "zai",
} as const;

export const moonshotaiProvider: ProviderDefinition = {
	id: "moonshotai",
	models: [
		defineModel({
			id: "kimi-k2.5",
			name: "Kimi K2.5",
			api: "openai-completions",
			provider: "moonshotai",
			baseUrl: MOONSHOT_BASE_URL,
			reasoning: true,
			input: ["text", "image"],
			cost: { input: 0.6, output: 3, cacheRead: 0.1, cacheWrite: 0 },
			contextWindow: 262144,
			maxTokens: 32768,
			compat: MOONSHOT_THINKING_COMPAT,
		}),
		// ... kimi-k2-0905-preview, kimi-k2-thinking, kimi-k2-thinking-turbo, kimi-k2-turbo-preview
	],
};
```
