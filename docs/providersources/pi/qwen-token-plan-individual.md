# Qwen Token Plan Individual (`qwen-token-plan-individual`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `qwen-token-plan-individual`
- **Name**: Qwen Token Plan Individual
- **API Protocol**: `openai-completions` (`POST /chat/completions`)
- **Base URL**: `https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1`
- **Source Files**:
  - `packages/ai/src/providers/qwen-token-plan-individual.ts`
  - `packages/ai/src/providers/qwen-token-plan-individual.models.ts`

## 2. Authentication & Compatibility
- **Environment Variable**: `QWEN_TOKEN_PLAN_API_KEY`
- **Authorization Header**: `Authorization: Bearer <QWEN_TOKEN_PLAN_API_KEY>`
- **Thinking Format**: Uses `thinkingFormat: "qwen"` (`enable_thinking: boolean`).

## 3. Source Implementation (`packages/ai/src/providers/qwen-token-plan-individual.ts`)
```typescript
import { openAICompletionsApi } from "../api/openai-completions.lazy.ts";
import { envApiKeyAuth } from "../auth/helpers.ts";
import { createProvider, type Provider } from "../models.ts";
import { QWEN_TOKEN_PLAN_INDIVIDUAL_MODELS } from "./qwen-token-plan-individual.models.ts";

export function qwenTokenPlanIndividualProvider(): Provider<"openai-completions"> {
	return createProvider({
		id: "qwen-token-plan-individual",
		name: "Qwen Token Plan Individual",
		baseUrl: "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
		auth: { apiKey: envApiKeyAuth("Qwen Token Plan Individual API key", ["QWEN_TOKEN_PLAN_API_KEY"]) },
		models: Object.values(QWEN_TOKEN_PLAN_INDIVIDUAL_MODELS),
		api: openAICompletionsApi(),
	});
}
```
