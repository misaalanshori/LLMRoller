# Xiaomi Token Plan AMS (`xiaomi-token-plan-ams`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `xiaomi-token-plan-ams`
- **Name**: Xiaomi Token Plan AMS (Europe / Amsterdam Endpoint)
- **API Protocol**: `openai-completions` (`POST /v1/chat/completions`)
- **Base URL**: `https://token-plan-ams.xiaomimimo.com/v1`
- **Source Files**:
  - `packages/ai/src/providers/xiaomi-token-plan-ams.ts`
  - `packages/ai/src/providers/xiaomi-token-plan-ams.models.ts`

## 2. Authentication & Compatibility
- **Environment Variable**: `XIAOMI_TOKEN_PLAN_AMS_API_KEY`
- **Authorization Header**: `Authorization: Bearer <XIAOMI_TOKEN_PLAN_AMS_API_KEY>`

## 3. Source Implementation (`packages/ai/src/providers/xiaomi-token-plan-ams.ts`)
```typescript
import { openAICompletionsApi } from "../api/openai-completions.lazy.ts";
import { envApiKeyAuth } from "../auth/helpers.ts";
import { createProvider, type Provider } from "../models.ts";
import { XIAOMI_TOKEN_PLAN_AMS_MODELS } from "./xiaomi-token-plan-ams.models.ts";

export function xiaomiTokenPlanAmsProvider(): Provider<"openai-completions"> {
	return createProvider({
		id: "xiaomi-token-plan-ams",
		name: "Xiaomi Token Plan AMS",
		baseUrl: "https://token-plan-ams.xiaomimimo.com/v1",
		auth: { apiKey: envApiKeyAuth("Xiaomi Token Plan AMS API key", ["XIAOMI_TOKEN_PLAN_AMS_API_KEY"]) },
		models: Object.values(XIAOMI_TOKEN_PLAN_AMS_MODELS),
		api: openAICompletionsApi(),
	});
}
```
