# Cerebras (`cerebras`) Provider Specification (`pi`)

## 1. Overview & Identification

- **Provider ID**: `cerebras`
- **Display Name**: `Cerebras`
- **Provider Module**: `packages/ai/src/providers/cerebras.ts`
- **API Implementation**: `packages/ai/src/api/openai-completions.ts` (`openai-completions`)
- **Models Catalog**: `packages/ai/src/providers/cerebras.models.ts` (`CEREBRAS_MODELS`)

---

## 2. Endpoints & Authentication

| Parameter | Value |
|---|---|
| **Base URL** | `https://api.cerebras.ai/v1` |
| **Chat Completions Path** | `POST /chat/completions` |
| **Authentication** | `Authorization: Bearer <apiKey>` (env `CEREBRAS_API_KEY`) |

---

## 3. Implementation (`packages/ai/src/providers/cerebras.ts`)

```typescript
import { openAICompletionsApi } from "../api/openai-completions.lazy.ts";
import { envApiKeyAuth } from "../auth/helpers.ts";
import { createProvider, type Provider } from "../models.ts";
import { CEREBRAS_MODELS } from "./cerebras.models.ts";

export function cerebrasProvider(): Provider<"openai-completions"> {
	return createProvider({
		id: "cerebras",
		name: "Cerebras",
		baseUrl: "https://api.cerebras.ai/v1",
		auth: { apiKey: envApiKeyAuth("Cerebras API key", ["CEREBRAS_API_KEY"]) },
		models: Object.values(CEREBRAS_MODELS),
		api: openAICompletionsApi(),
	});
}
```
