# Ant Ling (`ant-ling`) Provider Specification (`pi`)

## 1. Overview & Identification

- **Provider ID**: `ant-ling`
- **Display Name**: `Ant Ling`
- **Provider Module**: `packages/ai/src/providers/ant-ling.ts`
- **API Implementation**: `packages/ai/src/api/openai-completions.ts` (`openai-completions`)
- **Models Catalog**: `packages/ai/src/providers/ant-ling.models.ts` (`ANT_LING_MODELS`)

---

## 2. Endpoints & Authentication

| Parameter | Value |
|---|---|
| **Base URL** | `https://api.ant-ling.com/v1` |
| **Chat Completions Path** | `POST /chat/completions` |
| **Authentication** | `Authorization: Bearer <apiKey>` (env `ANT_LING_API_KEY`) |

---

## 3. Implementation (`packages/ai/src/providers/ant-ling.ts`)

```typescript
import { openAICompletionsApi } from "../api/openai-completions.lazy.ts";
import { envApiKeyAuth } from "../auth/helpers.ts";
import { createProvider, type Provider } from "../models.ts";
import { ANT_LING_MODELS } from "./ant-ling.models.ts";

export function antLingProvider(): Provider<"openai-completions"> {
	return createProvider({
		id: "ant-ling",
		name: "Ant Ling",
		baseUrl: "https://api.ant-ling.com/v1",
		auth: { apiKey: envApiKeyAuth("Ant Ling API key", ["ANT_LING_API_KEY"]) },
		models: Object.values(ANT_LING_MODELS),
		api: openAICompletionsApi(),
	});
}
```
