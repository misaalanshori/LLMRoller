# MiniMax (`minimax`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `minimax`
- **API Protocol**: `anthropic-messages` (`POST /v1/messages`)
- **Base URL**: `https://api.minimax.io/anthropic`
- **Source Files**:
  - `packages/ai/src/providers/minimax.ts`
  - `packages/ai/src/api/anthropic-messages.ts`
  - `packages/ai/src/env-api-keys.ts`

## 2. Authentication & Models
- **Environment Variable**: `MINIMAX_API_KEY`
- **Headers**: Standard Anthropic SDK headers (`x-api-key: <MINIMAX_API_KEY>`, `anthropic-version: 2023-06-01`).
- **Models**: `MiniMax-M2`, `MiniMax-M2.1`, `MiniMax-M2.1-highspeed`, `MiniMax-M2.5`, `MiniMax-M2.5-highspeed`, `MiniMax-M2.7`, `MiniMax-M2.7-highspeed` (all 196,608 context window, `reasoning: true`).

## 3. Source Implementation (`packages/ai/src/providers/minimax.ts`)
```typescript
const MINIMAX_BASE_URL = "https://api.minimax.io/anthropic";

export const minimaxProvider: ProviderDefinition = {
	id: "minimax",
	models: [
		defineModel({
			id: "MiniMax-M2.7",
			name: "MiniMax-M2.7",
			api: "anthropic-messages",
			provider: "minimax",
			baseUrl: MINIMAX_BASE_URL,
			reasoning: true,
			input: ["text"],
			cost: { input: 0.3, output: 1.2, cacheRead: 0.06, cacheWrite: 0.375 },
			contextWindow: 204800,
			maxTokens: 131072,
		}),
		// ... MiniMax-M2, MiniMax-M2.1, MiniMax-M2.5, and -highspeed variants
	],
};
```
