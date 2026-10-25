# Xiaomi Token Plan CN (`xiaomi-token-plan-cn`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `xiaomi-token-plan-cn`
- **Name**: Xiaomi Token Plan CN (Mainland China Endpoint)
- **API Protocol**: `openai-completions` (`POST /v1/chat/completions`)
- **Base URL**: `https://token-plan-cn.xiaomimimo.com/v1`
- **Source Files**:
  - `packages/ai/src/providers/xiaomi-token-plan-cn.ts`
  - `packages/ai/src/providers/xiaomi-token-plan-cn.models.ts`

## 2. Authentication & Compatibility
- **Environment Variable**: `XIAOMI_TOKEN_PLAN_CN_API_KEY`
- **Authorization Header**: `Authorization: Bearer <XIAOMI_TOKEN_PLAN_CN_API_KEY>`

## 3. Source Implementation (`packages/ai/src/providers/xiaomi-token-plan-cn.ts`)
```typescript
import { openAICompletionsApi } from "../api/openai-completions.lazy.ts";
import { envApiKeyAuth } from "../auth/helpers.ts";
import { createProvider, type Provider } from "../models.ts";
import { XIAOMI_TOKEN_PLAN_CN_MODELS } from "./xiaomi-token-plan-cn.models.ts";

export function xiaomiTokenPlanCnProvider(): Provider<"openai-completions"> {
	return createProvider({
		id: "xiaomi-token-plan-cn",
		name: "Xiaomi Token Plan CN",
		baseUrl: "https://token-plan-cn.xiaomimimo.com/v1",
		auth: { apiKey: envApiKeyAuth("Xiaomi Token Plan CN API key", ["XIAOMI_TOKEN_PLAN_CN_API_KEY"]) },
		models: Object.values(XIAOMI_TOKEN_PLAN_CN_MODELS),
		api: openAICompletionsApi(),
	});
}
```
