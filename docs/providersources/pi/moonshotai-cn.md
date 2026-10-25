# Moonshot AI China (`moonshotai-cn`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `moonshotai-cn`
- **API Protocol**: `openai-completions` (`POST /v1/chat/completions`)
- **Base URL**: `https://api.moonshot.cn/v1`
- **Source Files**:
  - `packages/ai/src/providers/moonshotai-cn.ts`
  - `packages/ai/src/api/openai-completions.ts`
  - `packages/ai/src/env-api-keys.ts`

## 2. Authentication & Compatibility
- **Environment Variable**: `MOONSHOT_CN_API_KEY` (fallback `MOONSHOT_API_KEY`)
- **Authorization Header**: `Authorization: Bearer <MOONSHOT_CN_API_KEY>`
- **Compatibility Flags**: `supportsDeveloperRole: false` and `thinkingFormat: "zai"` (`thinking: { type: "enabled" | "disabled" }`) for Kimi K2.5 and Kimi K2 Thinking models.

## 3. Source Implementation (`packages/ai/src/providers/moonshotai-cn.ts`)
```typescript
const MOONSHOT_CN_BASE_URL = "https://api.moonshot.cn/v1";

const MOONSHOT_COMPAT = {
	supportsDeveloperRole: false,
} as const;

const MOONSHOT_THINKING_COMPAT = {
	supportsDeveloperRole: false,
	thinkingFormat: "zai",
} as const;

export const moonshotaiCnProvider: ProviderDefinition = {
	id: "moonshotai-cn",
	models: [
		defineModel({
			id: "kimi-k2.5",
			name: "Kimi K2.5",
			api: "openai-completions",
			provider: "moonshotai-cn",
			baseUrl: MOONSHOT_CN_BASE_URL,
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
