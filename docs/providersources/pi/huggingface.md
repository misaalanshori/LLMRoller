# Hugging Face (`huggingface`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `huggingface`
- **API Protocol**: `openai-completions` (`POST /v1/chat/completions`)
- **Base URL**: `https://router.huggingface.co/v1`
- **Source Files**:
  - `packages/ai/src/providers/huggingface.ts`
  - `packages/ai/src/api/openai-completions.ts`
  - `packages/ai/src/env-api-keys.ts`

## 2. Authentication & Compatibility
- **Environment Variable**: `HF_TOKEN`
- **Authorization Header**: `Authorization: Bearer <HF_TOKEN>`
- **Compatibility Flags**: All models in `huggingface.ts` declare `compat: { supportsDeveloperRole: false }` so system messages use `"role": "system"` rather than `"developer"`.

## 3. Source Implementation (`packages/ai/src/providers/huggingface.ts`)
```typescript
const HUGGINGFACE_BASE_URL = "https://router.huggingface.co/v1";

const HUGGINGFACE_COMPAT = {
	supportsDeveloperRole: false,
} as const;

export const huggingfaceProvider: ProviderDefinition = {
	id: "huggingface",
	models: [
		defineModel({
			id: "deepseek-ai/DeepSeek-R1-0528",
			name: "DeepSeek-R1-0528",
			api: "openai-completions",
			provider: "huggingface",
			baseUrl: HUGGINGFACE_BASE_URL,
			reasoning: true,
			input: ["text"],
			cost: { input: 3, output: 5, cacheRead: 0, cacheWrite: 0 },
			contextWindow: 163840,
			maxTokens: 163840,
			compat: HUGGINGFACE_COMPAT,
		}),
		// ... DeepSeek-V3.2, Kimi-K2.5, Qwen3-235B-A22B-Thinking-2507, Qwen3-Coder-480B-A35B-Instruct, etc.
	],
};
```
