# xAI (`xai`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `xai`
- **Name**: xAI
- **API Protocol**: `openai-responses` (`POST /v1/responses`)
- **Base URL**: `https://api.x.ai/v1`
- **Source Files**:
  - `packages/ai/src/providers/xai.ts`
  - `packages/ai/src/providers/xai.models.ts`
  - `packages/ai/src/auth/oauth/xai.ts`
  - `packages/ai/src/api/openai-responses.ts`

## 2. Authentication (API Key & SuperGrok / X Premium Device OAuth)
1. **API Key**: `XAI_API_KEY` (`Authorization: Bearer <XAI_API_KEY>`).
2. **OAuth Device Flow (`packages/ai/src/auth/oauth/xai.ts`)**:
   - **Client ID**: `b1a00492-073a-47ea-816f-4c329264a828`
   - **Scope**: `openid profile email offline_access grok-cli:access api:access`
   - **Device Code URL**: `POST https://auth.x.ai/oauth2/device/code` (`application/x-www-form-urlencoded`)
   - **Token URL**: `POST https://auth.x.ai/oauth2/token`

## 3. Source Implementation (`packages/ai/src/providers/xai.ts`)
```typescript
import { openAIResponsesApi } from "../api/openai-responses.lazy.ts";
import { envApiKeyAuth, lazyOAuth } from "../auth/helpers.ts";
import { loadXaiOAuth } from "../auth/oauth/load.ts";
import { createProvider, type Provider } from "../models.ts";
import { XAI_MODELS } from "./xai.models.ts";

export function xaiProvider(): Provider<"openai-responses"> {
	return createProvider({
		id: "xai",
		name: "xAI",
		baseUrl: "https://api.x.ai/v1",
		auth: {
			apiKey: envApiKeyAuth("xAI API key", ["XAI_API_KEY"]),
			oauth: lazyOAuth({
				name: "xAI (Grok/X subscription)",
				isSubscription: true,
				loginLabel: "Sign in with SuperGrok or X Premium",
				load: loadXaiOAuth,
			}),
		},
		models: Object.values(XAI_MODELS),
		api: openAIResponsesApi(),
	});
}
```
