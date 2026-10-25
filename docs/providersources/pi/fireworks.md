# Fireworks (`fireworks`) Provider Specification (`pi`)

## 1. Overview & Identification

- **Provider ID**: `fireworks`
- **Display Name**: `Fireworks`
- **Provider Module**: `packages/ai/src/providers/fireworks.ts`
- **APIs**:
  - `packages/ai/src/api/anthropic-messages.ts` (`anthropic-messages`)
  - `packages/ai/src/api/openai-completions.ts` (`openai-completions`)
- **Models Catalog**: `packages/ai/src/providers/fireworks.models.ts` (`FIREWORKS_MODELS`)

---

## 2. Endpoints & Authentication

| Parameter | Value |
|---|---|
| **Base URL** | `https://api.fireworks.ai/inference` (models specify `/v1` or Anthropic base URLs) |
| **Authentication** | `Authorization: Bearer <apiKey>` or `x-api-key: <apiKey>` (env `FIREWORKS_API_KEY`) |
| **Supported APIs** | `"anthropic-messages"`, `"openai-completions"` |

---

## 3. Implementation (`packages/ai/src/providers/fireworks.ts`)

```typescript
import { anthropicMessagesApi } from "../api/anthropic-messages.lazy.ts";
import { openAICompletionsApi } from "../api/openai-completions.lazy.ts";
import { envApiKeyAuth } from "../auth/helpers.ts";
import { createProvider, type Provider } from "../models.ts";
import { FIREWORKS_MODELS } from "./fireworks.models.ts";

export function fireworksProvider(): Provider<"anthropic-messages" | "openai-completions"> {
	return createProvider({
		id: "fireworks",
		name: "Fireworks",
		baseUrl: "https://api.fireworks.ai/inference",
		auth: { apiKey: envApiKeyAuth("Fireworks API key", ["FIREWORKS_API_KEY"]) },
		models: Object.values(FIREWORKS_MODELS),
		api: {
			"anthropic-messages": anthropicMessagesApi(),
			"openai-completions": openAICompletionsApi(),
		},
	});
}
```
