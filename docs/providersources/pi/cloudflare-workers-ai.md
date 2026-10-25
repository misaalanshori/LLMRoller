# Cloudflare Workers AI (`cloudflare-workers-ai`) Provider Specification (`pi`)

## 1. Overview & Identification

- **Provider ID**: `cloudflare-workers-ai`
- **Display Name**: `Cloudflare Workers AI`
- **Provider Modules**:
  - `packages/ai/src/providers/cloudflare-workers-ai.ts`
  - `packages/ai/src/providers/cloudflare-auth.ts`
  - `packages/ai/src/providers/cloudflare-stream.ts`
- **APIs**:
  - `packages/ai/src/api/openai-completions.ts` (`openai-completions`)
  - `packages/ai/src/api/cloudflare-workers-ai-system-one.ts` (`cloudflare-workers-ai-system-one`)
- **Models Catalog**: `packages/ai/src/providers/cloudflare-workers-ai.models.ts` (`CLOUDFLARE_WORKERS_AI_MODELS`, `CLOUDFLARE_WORKERS_AI_CLASSIFIER_MODELS`)

---

## 2. Endpoints & Authentication

| Service | Endpoint Template |
|---|---|
| **OpenAI-Compatible Chat** | `https://api.cloudflare.com/client/v4/accounts/{CLOUDFLARE_ACCOUNT_ID}/ai/v1/chat/completions` |
| **System One Classifier Run API** | `POST https://api.cloudflare.com/client/v4/accounts/{CLOUDFLARE_ACCOUNT_ID}/ai/run` (`{ model: model.id, input: request }`) |
| **Authentication** | `Authorization: Bearer <CLOUDFLARE_API_KEY>` + `CLOUDFLARE_ACCOUNT_ID` |

---

## 3. Implementation (`packages/ai/src/providers/cloudflare-workers-ai.ts` & `cloudflare-workers-ai-system-one.ts`)

```typescript
export function cloudflareWorkersAIProvider(): Provider<"openai-completions"> {
	return createProvider<"openai-completions">({
		id: "cloudflare-workers-ai",
		name: "Cloudflare Workers AI",
		auth: { apiKey: cloudflareWorkersAIAuth() },
		models: [
			...Object.values(CLOUDFLARE_WORKERS_AI_MODELS),
			...Object.values(CLOUDFLARE_WORKERS_AI_CLASSIFIER_MODELS),
		],
		api: cloudflareStreams(openAICompletionsApi()),
		classifiers: {
			"cloudflare-workers-ai-system-one": cloudflareClassifier(cloudflareWorkersAISystemOneApi()),
		},
	});
}
```
- In `cloudflare-workers-ai-system-one.ts`, `@cf/cloudflare/clef*` models pass images in `images` (list of data URLs) and return `{ success, result: { model, answers, usage } }`, whereas third-party models like `typesafe/jev` pass images as System One `assets` and return `{ success, result: { state: "Completed", result: { answers, usage } } }`.
