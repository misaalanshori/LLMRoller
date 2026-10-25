# Meta (`meta`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `meta`
- **API Protocol**: `openai-responses` (`POST /v1/responses`)
- **Base URL**: `https://api.meta.ai/v1`
- **Source Files**:
  - `packages/ai/src/providers/meta.ts`
  - `packages/ai/src/auth/oauth/meta.ts`
  - `packages/ai/src/api/openai-responses.ts`

## 2. Authentication (API Key & OIDC Device Flow + Key Minting)
- **Environment Variable**: `META_API_KEY`
- **OIDC Device Flow (`packages/ai/src/auth/oauth/meta.ts`)**:
  - **Client ID**: `1031625952748946`
  - **Device Authorization Endpoint**: `POST https://auth.meta.com/oidc/device/authorization/`
  - **Token Endpoint**: `POST https://auth.meta.com/oidc/device/token/`
  - **Model API Key Minting**: Exchanges OIDC `access_token` at `POST https://api.meta.ai/muse-code/key` (`Authorization: Bearer <oidc_access_token>`) for a 24-hour `api_key` used as the Bearer token against `https://api.meta.ai/v1/responses`.

## 3. Source Implementation (`packages/ai/src/providers/meta.ts` & `packages/ai/src/auth/oauth/meta.ts`)
```typescript
const META_BASE_URL = "https://api.meta.ai/v1";

export const metaProvider: ProviderDefinition = {
	id: "meta",
	models: [
		defineModel({
			id: "meta-muse-spark",
			name: "Muse Spark",
			api: "openai-responses",
			provider: "meta",
			baseUrl: META_BASE_URL,
			reasoning: true,
			input: ["text", "image"],
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
			contextWindow: 1048576,
			maxTokens: 128000,
		}),
	],
};
```
