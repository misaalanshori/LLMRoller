# Z.AI Coding CN (`zai-coding-cn`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `zai-coding-cn`
- **Name**: Z.AI Coding CN (Zhipu BigModel Mainland China Coding Endpoint)
- **API Protocol**: `openai-completions` (`POST /chat/completions`)
- **Base URL**: `https://open.bigmodel.cn/api/coding/paas/v4`
- **Source Files**:
  - `packages/ai/src/providers/zai-coding-cn.ts`
  - `packages/ai/src/providers/zai-coding-cn.models.ts`
  - `packages/ai/src/api/openai-completions.ts`

## 2. Authentication & Compatibility
- **Environment Variable**: `ZAI_CODING_CN_API_KEY`
- **Authorization Header**: `Authorization: Bearer <ZAI_CODING_CN_API_KEY>`
- **Compatibility Flags**: `supportsDeveloperRole: false` and `thinkingFormat: "zai"` (`thinking: { type: "enabled" | "disabled" }`).

## 3. Source Implementation (`packages/ai/src/providers/zai-coding-cn.ts`)
```typescript
import { openAICompletionsApi } from "../api/openai-completions.lazy.ts";
import { envApiKeyAuth } from "../auth/helpers.ts";
import { createProvider, type Provider } from "../models.ts";
import { ZAI_CODING_CN_MODELS } from "./zai-coding-cn.models.ts";

export function zaiCodingCnProvider(): Provider<"openai-completions"> {
	return createProvider({
		id: "zai-coding-cn",
		name: "Z.AI Coding CN",
		baseUrl: "https://open.bigmodel.cn/api/coding/paas/v4",
		auth: { apiKey: envApiKeyAuth("Z.AI Coding CN API key", ["ZAI_CODING_CN_API_KEY"]) },
		models: Object.values(ZAI_CODING_CN_MODELS),
		api: openAICompletionsApi(),
	});
}
```
