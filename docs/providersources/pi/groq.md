# Groq (`groq`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `groq`
- **API Protocol**: `openai-completions` (`POST /v1/chat/completions`)
- **Base URL**: `https://api.groq.com/openai/v1`
- **Source Files**:
  - `packages/ai/src/providers/groq.ts`
  - `packages/ai/src/api/openai-completions.ts`
  - `packages/ai/src/env-api-keys.ts`

## 2. Authentication & Compatibility
- **Environment Variable**: `GROQ_API_KEY`
- **Authorization Header**: `Authorization: Bearer <GROQ_API_KEY>`
- **Compatibility Detection**:
  - `maxTokensField`: `"max_tokens"` (detected in `detectCompat` when `provider === "groq"`).
  - Reasoning models (`openai/gpt-oss-*` and `qwen/qwen3-32b`) map reasoning effort using `thinkingFormat: "openai"` (`reasoning_effort`). For `qwen/qwen3-32b`, `openai-completions.ts` maps all non-off levels to `"default"`.

## 3. Source Implementation (`packages/ai/src/providers/groq.ts`)
```typescript
const GROQ_BASE_URL = "https://api.groq.com/openai/v1";

export const groqProvider: ProviderDefinition = {
	id: "groq",
	models: [
		defineModel({
			id: "llama-3.3-70b-versatile",
			name: "Llama 3.3 70B Versatile",
			api: "openai-completions",
			provider: "groq",
			baseUrl: GROQ_BASE_URL,
			reasoning: false,
			input: ["text"],
			cost: { input: 0.59, output: 0.79, cacheRead: 0, cacheWrite: 0 },
			contextWindow: 131072,
			maxTokens: 32768,
		}),
		// ... meta-llama/llama-4-maverick-17b-128e-instruct, meta-llama/llama-4-scout-17b-16e-instruct,
		// moonshotai/kimi-k2-instruct-0905, openai/gpt-oss-120b, openai/gpt-oss-20b, qwen/qwen3-32b
	],
};
```
