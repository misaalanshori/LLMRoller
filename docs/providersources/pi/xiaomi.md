# Xiaomi MiMo (`xiaomi`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `xiaomi`
- **Name**: Xiaomi
- **API Protocol**: `openai-completions` (`POST /v1/chat/completions`)
- **Base URL**: `https://api.xiaomimimo.com/v1`
- **Source Files**:
  - `packages/ai/src/providers/xiaomi.ts`
  - `packages/ai/src/providers/xiaomi.models.ts`
  - `packages/ai/src/api/openai-completions.ts`

## 2. Authentication & Compatibility
- **Environment Variable**: `XIAOMI_API_KEY`
- **Authorization Header**: `Authorization: Bearer <XIAOMI_API_KEY>`
- **Compatibility Flags**: `supportsDeveloperRole: false`, `thinkingFormat: "zai"` (`thinking: { type: "enabled" | "disabled" }`) on Xiaomi MiMo reasoning models (`mimo-v2-flash`, `mimo-v2-pro`, `mimo-v2-omni`).

## 3. Source Implementation (`packages/ai/src/providers/xiaomi.ts`)
```typescript
import { openAICompletionsApi } from "../api/openai-completions.lazy.ts";
import { envApiKeyAuth } from "../auth/helpers.ts";
import { createProvider, type Provider } from "../models.ts";
import { XIAOMI_MODELS } from "./xiaomi.models.ts";

export function xiaomiProvider(): Provider<"openai-completions"> {
	return createProvider({
		id: "xiaomi",
		name: "Xiaomi",
		baseUrl: "https://api.xiaomimimo.com/v1",
		auth: { apiKey: envApiKeyAuth("Xiaomi API key", ["XIAOMI_API_KEY"]) },
		models: Object.values(XIAOMI_MODELS),
		api: openAICompletionsApi(),
	});
}
```
