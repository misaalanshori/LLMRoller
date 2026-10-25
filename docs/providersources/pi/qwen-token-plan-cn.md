# Qwen Token Plan CN (`qwen-token-plan-cn`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `qwen-token-plan-cn`
- **Name**: Qwen Token Plan CN (Alibaba Cloud DashScope Beijing Token Plan)
- **API Protocol**: `openai-completions` (`POST /chat/completions`)
- **Base URL**: `https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1`
- **Source Files**:
  - `packages/ai/src/providers/qwen-token-plan-cn.ts`
  - `packages/ai/src/providers/qwen-token-plan-cn.models.ts`

## 2. Authentication & Compatibility
- **Environment Variable**: `QWEN_TOKEN_PLAN_CN_API_KEY`
- **Authorization Header**: `Authorization: Bearer <QWEN_TOKEN_PLAN_CN_API_KEY>`
- **Thinking Format**: Uses `thinkingFormat: "qwen"` (`enable_thinking: boolean`).

## 3. Source Implementation (`packages/ai/src/providers/qwen-token-plan-cn.ts`)
```typescript
import { openAICompletionsApi } from "../api/openai-completions.lazy.ts";
import { envApiKeyAuth } from "../auth/helpers.ts";
import { createProvider, type Provider } from "../models.ts";
import { QWEN_TOKEN_PLAN_CN_MODELS } from "./qwen-token-plan-cn.models.ts";

export function qwenTokenPlanCnProvider(): Provider<"openai-completions"> {
	return createProvider({
		id: "qwen-token-plan-cn",
		name: "Qwen Token Plan CN",
		baseUrl: "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
		auth: { apiKey: envApiKeyAuth("Qwen Token Plan CN API key", ["QWEN_TOKEN_PLAN_CN_API_KEY"]) },
		models: Object.values(QWEN_TOKEN_PLAN_CN_MODELS),
		api: openAICompletionsApi(),
	});
}
```
