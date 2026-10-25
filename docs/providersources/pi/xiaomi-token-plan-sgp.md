# Xiaomi Token Plan SGP (`xiaomi-token-plan-sgp`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `xiaomi-token-plan-sgp`
- **Name**: Xiaomi Token Plan SGP (Singapore Endpoint)
- **API Protocol**: `openai-completions` (`POST /v1/chat/completions`)
- **Base URL**: `https://token-plan-sgp.xiaomimimo.com/v1`
- **Source Files**:
  - `packages/ai/src/providers/xiaomi-token-plan-sgp.ts`
  - `packages/ai/src/providers/xiaomi-token-plan-sgp.models.ts`

## 2. Authentication & Compatibility
- **Environment Variable**: `XIAOMI_TOKEN_PLAN_SGP_API_KEY`
- **Authorization Header**: `Authorization: Bearer <XIAOMI_TOKEN_PLAN_SGP_API_KEY>`

## 3. Source Implementation (`packages/ai/src/providers/xiaomi-token-plan-sgp.ts`)
```typescript
import { openAICompletionsApi } from "../api/openai-completions.lazy.ts";
import { envApiKeyAuth } from "../auth/helpers.ts";
import { createProvider, type Provider } from "../models.ts";
import { XIAOMI_TOKEN_PLAN_SGP_MODELS } from "./xiaomi-token-plan-sgp.models.ts";

export function xiaomiTokenPlanSgpProvider(): Provider<"openai-completions"> {
	return createProvider({
		id: "xiaomi-token-plan-sgp",
		name: "Xiaomi Token Plan SGP",
		baseUrl: "https://token-plan-sgp.xiaomimimo.com/v1",
		auth: { apiKey: envApiKeyAuth("Xiaomi Token Plan SGP API key", ["XIAOMI_TOKEN_PLAN_SGP_API_KEY"]) },
		models: Object.values(XIAOMI_TOKEN_PLAN_SGP_MODELS),
		api: openAICompletionsApi(),
	});
}
```
