# Qwen Token Plan (`qwen-token-plan`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `qwen-token-plan`
- **Name**: Qwen Token Plan (Alibaba Cloud Model Studio International / Singapore Coding Plan)
- **API Protocol**: `openai-completions` (`POST /chat/completions`)
- **Base URL**: `https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1`
- **Source Files**:
  - `packages/ai/src/providers/qwen-token-plan.ts`
  - `packages/ai/src/providers/qwen-token-plan.models.ts`

## 2. Authentication & Compatibility
- **Environment Variable**: `QWEN_TOKEN_PLAN_API_KEY`
- **Authorization Header**: `Authorization: Bearer <QWEN_TOKEN_PLAN_API_KEY>`
- **Thinking Format**: Uses `thinkingFormat: "qwen"` (`enable_thinking: true/false` top-level parameter in `openai-completions.ts`).

## 3. Source Implementation (`packages/ai/src/providers/qwen-token-plan.ts`)
```typescript
import { openAICompletionsApi } from "../api/openai-completions.lazy.ts";
import { envApiKeyAuth } from "../auth/helpers.ts";
import { createProvider, type Provider } from "../models.ts";
import { QWEN_TOKEN_PLAN_MODELS } from "./qwen-token-plan.models.ts";

export function qwenTokenPlanProvider(): Provider<"openai-completions"> {
	return createProvider({
		id: "qwen-token-plan",
		name: "Qwen Token Plan",
		baseUrl: "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
		auth: { apiKey: envApiKeyAuth("Qwen Token Plan API key", ["QWEN_TOKEN_PLAN_API_KEY"]) },
		models: Object.values(QWEN_TOKEN_PLAN_MODELS),
		api: openAICompletionsApi(),
	});
}
```
