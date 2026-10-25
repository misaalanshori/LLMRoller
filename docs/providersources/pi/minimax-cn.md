# MiniMax China (`minimax-cn`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `minimax-cn`
- **API Protocol**: `anthropic-messages` (`POST /v1/messages`)
- **Base URL**: `https://api.minimaxi.com/anthropic`
- **Source Files**:
  - `packages/ai/src/providers/minimax-cn.ts`
  - `packages/ai/src/api/anthropic-messages.ts`
  - `packages/ai/src/env-api-keys.ts`

## 2. Authentication & Models
- **Environment Variable**: `MINIMAX_CN_API_KEY`
- **Headers**: Standard Anthropic SDK headers (`x-api-key: <MINIMAX_CN_API_KEY>`, `anthropic-version: 2023-06-01`) routed to MiniMax's domestic China endpoint (`https://api.minimaxi.com/anthropic`).

## 3. Source Implementation (`packages/ai/src/providers/minimax-cn.ts`)
```typescript
const MINIMAX_CN_BASE_URL = "https://api.minimaxi.com/anthropic";

export const minimaxCnProvider: ProviderDefinition = {
	id: "minimax-cn",
	models: [
		defineModel({
			id: "MiniMax-M2.7",
			name: "MiniMax-M2.7",
			api: "anthropic-messages",
			provider: "minimax-cn",
			baseUrl: MINIMAX_CN_BASE_URL,
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
