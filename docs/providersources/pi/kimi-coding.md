# Kimi For Coding (`kimi-coding`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `kimi-coding`
- **API Protocol**: `anthropic-messages` (`POST /v1/messages`)
- **Base URL**: `https://api.kimi.com/coding`
- **Source Files**:
  - `packages/ai/src/providers/kimi-coding.ts`
  - `packages/ai/src/auth/oauth/kimi-coding.ts`
  - `packages/ai/src/api/anthropic-messages.ts`

## 2. Authentication (API Key & OAuth Device Flow)
- **Environment Variable**: `KIMI_API_KEY`
- **OAuth Device Flow (`packages/ai/src/auth/oauth/kimi-coding.ts`)**:
  - **Client ID**: `17e5f671-d194-4dfb-9706-5516cb48c098`
  - **Device Authorization Endpoint**: `POST https://auth.kimi.com/api/oauth/device_authorization`
  - **Token Endpoint**: `POST https://auth.kimi.com/api/oauth/token` (`grant_type=urn:ietf:params:oauth:grant-type:device_code` and `refresh_token`)
  - **Custom Headers**: `X-Msh-Platform: kimi_cli`, `X-Msh-Version`, `X-Msh-Device-Name`, `X-Msh-Device-Model`, `X-Msh-Os-Version`, `X-Msh-Device-Id`.

## 3. Source Implementation (`packages/ai/src/providers/kimi-coding.ts` & `packages/ai/src/auth/oauth/kimi-coding.ts`)
```typescript
const KIMI_CODING_BASE_URL = "https://api.kimi.com/coding";

export const kimiCodingProvider: ProviderDefinition = {
	id: "kimi-coding",
	models: [
		defineModel({
			id: "kimi-k2-thinking",
			name: "Kimi K2 Thinking",
			api: "anthropic-messages",
			provider: "kimi-coding",
			baseUrl: KIMI_CODING_BASE_URL,
			reasoning: true,
			input: ["text"],
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
			contextWindow: 262144,
			maxTokens: 32768,
		}),
		defineModel({
			id: "k2p5",
			name: "Kimi K2.5",
			api: "anthropic-messages",
			provider: "kimi-coding",
			baseUrl: KIMI_CODING_BASE_URL,
			reasoning: true,
			input: ["text", "image"],
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
			contextWindow: 262144,
			maxTokens: 32768,
		}),
	],
};
```
