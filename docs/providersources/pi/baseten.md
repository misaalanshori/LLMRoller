# Baseten (`baseten`) Provider Specification (`pi`)

## 1. Overview & Identification

- **Provider ID**: `baseten`
- **Display Name**: `Baseten`
- **Provider Module**: `packages/ai/src/providers/baseten.ts`
- **API Implementation**: `packages/ai/src/api/openai-completions.ts` (`openai-completions`)
- **Models Catalog**: `packages/ai/src/providers/baseten.models.ts` (`BASETEN_MODELS`)

---

## 2. Endpoints & Authentication

| Parameter | Value |
|---|---|
| **Base URL** | `https://inference.baseten.co/v1` |
| **Chat Completions Path** | `POST /chat/completions` |
| **Authentication** | `Authorization: Bearer <apiKey>` (env `BASETEN_API_KEY`) |

---

## 3. Implementation (`packages/ai/src/providers/baseten.ts`)

```typescript
import { openAICompletionsApi } from "../api/openai-completions.lazy.ts";
import { envApiKeyAuth } from "../auth/helpers.ts";
import { createProvider, type Provider } from "../models.ts";
import { BASETEN_MODELS } from "./baseten.models.ts";

export function basetenProvider(): Provider<"openai-completions"> {
	return createProvider({
		id: "baseten",
		name: "Baseten",
		baseUrl: "https://inference.baseten.co/v1",
		auth: { apiKey: envApiKeyAuth("Baseten API key", ["BASETEN_API_KEY"]) },
		models: Object.values(BASETEN_MODELS),
		api: openAICompletionsApi(),
	});
}
```
