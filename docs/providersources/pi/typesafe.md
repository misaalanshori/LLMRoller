# TypeSafe (`typesafe`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `typesafe`
- **Name**: TypeSafe
- **API Protocol**: `typesafe-system-one` (`POST <baseUrl>/systemone`)
- **Base URL**: `https://api.typesafe.ai/v1` (per model in `typesafe.models.ts`)
- **Source Files**:
  - `packages/ai/src/providers/typesafe.ts`
  - `packages/ai/src/providers/typesafe.models.ts`
  - `packages/ai/src/api/typesafe-system-one.ts`
  - `packages/ai/src/api/system-one-shared.ts`

## 2. Authentication & Protocol Behavior
- **Environment Variable**: `TYPESAFE_API_KEY`
- **Headers**:
  - `Authorization: Bearer <TYPESAFE_API_KEY>`
  - `TypeSafe-Danger-Zone-Images: true` (when passing `assets` image inputs to native TypeSafe endpoints).
- **Wire Protocol (`typesafe-system-one`)**:
  - Sends `POST <baseUrl>/systemone` with `{ model: model.id, state, questions, assets }`.
  - Maps boolean classifications (`bool`) to wire-level `noul` types.

## 3. Source Implementation (`packages/ai/src/providers/typesafe.ts` & `packages/ai/src/api/typesafe-system-one.ts`)
```typescript
export function typesafeProvider(): Provider {
	return createProvider({
		id: "typesafe",
		name: "TypeSafe",
		auth: {
			apiKey: envApiKeyAuth("TypeSafe API key", ["TYPESAFE_API_KEY"]),
		},
		models: Object.values(TYPESAFE_CLASSIFIER_MODELS),
		classifiers: {
			"typesafe-system-one": typesafeSystemOneApi(),
		},
	});
}

const transport: SystemOneTransport = {
	api: "typesafe-system-one",
	label: "System One API",
	detectCompat: (model) =>
		model.provider === "openrouter" || model.baseUrl.includes("openrouter.ai")
			? { imageInput: "state-image-urls", sendImageOptInHeader: false }
			: { imageInput: "assets", sendImageOptInHeader: true },
	url: (model) => new URL("systemone", `${model.baseUrl.replace(/\/+$/u, "")}/`),
	payload: (model, request) => ({ model: model.id, ...request }),
	output: (body) => {
		if (!isRecord(body)) throw new Error("System One API returned an unexpected response");
		return body;
	},
};
```
