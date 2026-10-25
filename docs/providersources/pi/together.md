# Together AI (`together`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `together`
- **Name**: Together
- **API Protocol**: `openai-completions` (`POST /v1/chat/completions`)
- **Base URL**: `https://api.together.ai/v1`
- **Source Files**:
  - `packages/ai/src/providers/together.ts`
  - `packages/ai/src/providers/together.models.ts`
  - `packages/ai/src/api/openai-completions.ts`

## 2. Authentication & Compatibility
- **Environment Variable**: `TOGETHER_API_KEY`
- **Authorization Header**: `Authorization: Bearer <TOGETHER_API_KEY>`
- **Compatibility Flags**: `supportsDeveloperRole: false` across hosted open-weight models (`deepseek-ai/DeepSeek-R1`, `deepseek-ai/DeepSeek-V3.1`, `moonshotai/Kimi-K2.5`, `Qwen/Qwen3.5-397B-A17B`, `zai-org/GLM-5`).

## 3. Source Implementation (`packages/ai/src/providers/together.ts`)
```typescript
import { openAICompletionsApi } from "../api/openai-completions.lazy.ts";
import { envApiKeyAuth } from "../auth/helpers.ts";
import { createProvider, type Provider } from "../models.ts";
import { TOGETHER_MODELS } from "./together.models.ts";

export function togetherProvider(): Provider<"openai-completions"> {
	return createProvider({
		id: "together",
		name: "Together",
		baseUrl: "https://api.together.ai/v1",
		auth: { apiKey: envApiKeyAuth("Together API key", ["TOGETHER_API_KEY"]) },
		models: Object.values(TOGETHER_MODELS),
		api: openAICompletionsApi(),
	});
}
```
