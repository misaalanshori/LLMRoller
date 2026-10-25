# NVIDIA (`nvidia`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `nvidia`
- **API Protocol**: `openai-completions` (`POST /v1/chat/completions`)
- **Base URL**: `https://integrate.api.nvidia.com/v1`
- **Source Files**:
  - `packages/ai/src/providers/nvidia.ts`
  - `packages/ai/src/api/openai-completions.ts`
  - `packages/ai/src/env-api-keys.ts`

## 2. Authentication & Compatibility
- **Environment Variable**: `NVIDIA_API_KEY`
- **Authorization Header**: `Authorization: Bearer <NVIDIA_API_KEY>`
- **Compatibility Flags**: `supportsDeveloperRole: false`, `thinkingFormat: "qwen-chat-template"` (`chat_template_kwargs: { enable_thinking: true }`) on reasoning models hosted on NVIDIA NIM (`deepseek-ai/deepseek-v3.2`, `moonshotai/kimi-k2.5`, `nvidia/nemotron-3-super-120b-a12b`, `qwen/qwen3.5-397b-a17b`, `z-ai/glm5`).

## 3. Source Implementation (`packages/ai/src/providers/nvidia.ts`)
```typescript
const NVIDIA_BASE_URL = "https://integrate.api.nvidia.com/v1";

export const nvidiaProvider: ProviderDefinition = {
	id: "nvidia",
	models: [
		defineModel({
			id: "nvidia/nemotron-3-super-120b-a12b",
			name: "Nemotron 3 Super 120B",
			api: "openai-completions",
			provider: "nvidia",
			baseUrl: NVIDIA_BASE_URL,
			reasoning: true,
			input: ["text"],
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
			contextWindow: 262144,
			maxTokens: 32768,
			compat: {
				supportsDeveloperRole: false,
				thinkingFormat: "qwen-chat-template",
			},
		}),
		// ... deepseek-ai/deepseek-v3.2, minimaxai/minimax-m2.5, moonshotai/kimi-k2.5, qwen/qwen3.5-397b-a17b, z-ai/glm5
	],
};
```
