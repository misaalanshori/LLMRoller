# Vercel AI Gateway (`vercel-ai-gateway`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `vercel-ai-gateway`
- **Name**: Vercel AI Gateway
- **API Protocols**:
  - Chat: `anthropic-messages` (`POST /v1/messages`)
  - Classifier: `typesafe-system-one` (`/typesafe/v1/systemone`)
- **Base URL**: `https://ai-gateway.vercel.sh`
- **Source Files**:
  - `packages/ai/src/providers/vercel-ai-gateway.ts`
  - `packages/ai/src/providers/vercel-ai-gateway.models.ts`
  - `packages/ai/src/api/anthropic-messages.ts`
  - `packages/ai/src/api/typesafe-system-one.ts`

## 2. Authentication & Protocol Behavior
- **Environment Variable**: `AI_GATEWAY_API_KEY`
- **Headers**: Authenticated via Anthropic Messages SDK (`x-api-key` or `Authorization: Bearer <AI_GATEWAY_API_KEY>`) against `https://ai-gateway.vercel.sh`.
- **Classifier Endpoint**: Routes `typesafe-system-one` classifier models to `https://ai-gateway.vercel.sh/typesafe/v1/systemone`.

## 3. Source Implementation (`packages/ai/src/providers/vercel-ai-gateway.ts`)
```typescript
import { anthropicMessagesApi } from "../api/anthropic-messages.lazy.ts";
import { typesafeSystemOneApi } from "../api/typesafe-system-one.lazy.ts";
import { envApiKeyAuth } from "../auth/helpers.ts";
import { createProvider, type Provider } from "../models.ts";
import { VERCEL_AI_GATEWAY_CLASSIFIER_MODELS, VERCEL_AI_GATEWAY_MODELS } from "./vercel-ai-gateway.models.ts";

export function vercelAIGatewayProvider(): Provider<"anthropic-messages"> {
	return createProvider<"anthropic-messages">({
		id: "vercel-ai-gateway",
		name: "Vercel AI Gateway",
		baseUrl: "https://ai-gateway.vercel.sh",
		auth: { apiKey: envApiKeyAuth("Vercel AI Gateway API key", ["AI_GATEWAY_API_KEY"]) },
		models: [...Object.values(VERCEL_AI_GATEWAY_MODELS), ...Object.values(VERCEL_AI_GATEWAY_CLASSIFIER_MODELS)],
		api: anthropicMessagesApi(),
		// AI Gateway serves TypeSafe's System One protocol at /typesafe/v1/systemone.
		classifiers: { "typesafe-system-one": typesafeSystemOneApi() },
	});
}
```
