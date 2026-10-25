# Z.AI (`zai`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `zai`
- **Name**: Z.AI
- **API Protocol**: `openai-completions` (`POST /chat/completions`)
- **Base URL**: `https://api.z.ai/api/coding/paas/v4`
- **Source Files**:
  - `packages/ai/src/providers/zai.ts`
  - `packages/ai/src/providers/zai.models.ts`
  - `packages/ai/src/api/openai-completions.ts`

## 2. Authentication & Compatibility
- **Environment Variable**: `ZAI_API_KEY`
- **Authorization Header**: `Authorization: Bearer <ZAI_API_KEY>`
- **Compatibility Detection (`detectCompat` in `openai-completions.ts`)**:
  - `supportsDeveloperRole: false` (detected via `provider === "zai"` or `baseUrl.includes("api.z.ai")`)
  - `thinkingFormat: "zai"` (`thinking: { type: "enabled" | "disabled" }`).

## 3. Source Implementation (`packages/ai/src/providers/zai.ts`)
```typescript
import { openAICompletionsApi } from "../api/openai-completions.lazy.ts";
import { envApiKeyAuth } from "../auth/helpers.ts";
import { createProvider, type Provider } from "../models.ts";
import { ZAI_MODELS } from "./zai.models.ts";

export function zaiProvider(): Provider<"openai-completions"> {
	return createProvider({
		id: "zai",
		name: "Z.AI",
		baseUrl: "https://api.z.ai/api/coding/paas/v4",
		auth: { apiKey: envApiKeyAuth("Z.AI API key", ["ZAI_API_KEY"]) },
		models: Object.values(ZAI_MODELS),
		api: openAICompletionsApi(),
	});
}
```
