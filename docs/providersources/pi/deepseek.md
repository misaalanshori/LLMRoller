# DeepSeek (`deepseek`) Provider Specification (`pi`)

## 1. Overview & Identification

- **Provider ID**: `deepseek`
- **Display Name**: `DeepSeek`
- **Provider Module**: `packages/ai/src/providers/deepseek.ts`
- **API Implementation**: `packages/ai/src/api/openai-completions.ts` (`openai-completions`)
- **Models Catalog**: `packages/ai/src/providers/deepseek.models.ts` (`DEEPSEEK_MODELS`)

---

## 2. Endpoints & Authentication

| Parameter | Value |
|---|---|
| **Base URL** | `https://api.deepseek.com` |
| **Chat Completions Path** | `POST /chat/completions` |
| **Authentication** | `Authorization: Bearer <apiKey>` (env `DEEPSEEK_API_KEY`) |

---

## 3. Implementation (`packages/ai/src/providers/deepseek.ts`)

```typescript
import { openAICompletionsApi } from "../api/openai-completions.lazy.ts";
import { envApiKeyAuth } from "../auth/helpers.ts";
import { createProvider, type Provider } from "../models.ts";
import { DEEPSEEK_MODELS } from "./deepseek.models.ts";

export function deepseekProvider(): Provider<"openai-completions"> {
	return createProvider({
		id: "deepseek",
		name: "DeepSeek",
		baseUrl: "https://api.deepseek.com",
		auth: { apiKey: envApiKeyAuth("DeepSeek API key", ["DEEPSEEK_API_KEY"]) },
		models: Object.values(DEEPSEEK_MODELS),
		api: openAICompletionsApi(),
	});
}
```
