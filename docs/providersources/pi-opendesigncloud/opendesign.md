# OpenDesign (`opendesign`) Provider Specification (`pi-opendesigncloud`)

## 1. Overview
- **Provider ID**: `opendesign`
- **Provider Name**: `OpenDesign`
- **API Protocol**: `openai-completions` (`POST /v1/chat/completions`, `GET /v1/models`)
- **Base URL**: `https://amr-link.open-design.ai/v1`
- **Source Files**:
  - `index.ts`
  - `models.ts`

## 2. Authentication & Endpoints
- **Environment Variable**: `OPENDESIGN_API_KEY` (`apiKey: "$OPENDESIGN_API_KEY"`)
- **HTTP Headers**:
  - `Authorization: Bearer <OPENDESIGN_API_KEY>`
- **Endpoints**:
  - **Chat Completions**: `POST https://amr-link.open-design.ai/v1/chat/completions` (handled via Pi's `openai-completions` protocol)
  - **Dynamic Model Catalog**: `GET https://amr-link.open-design.ai/v1/models`

## 3. Dynamic Catalog Parsing & Default Models (`models.ts`)
- `refreshOpenDesignModels` fetches `GET ${baseUrl}/models` with `Authorization: Bearer ${apiKey}` when `context.allowNetwork` is true.
- Parses `json.data` entries (`OpenDesignApiModel`):
  - Skips models where `item.enabled === false`.
  - Extracts `input_modalities` from `item.architecture.input_modalities` (`["text", "image"]` if `"image"` is present, else `["text"]`).
  - Extracts `contextWindow` from `item.metadata.context_limit` (default `128000`) and `maxTokens` from `item.metadata.output_budget` (default `32000`).
  - Sets `reasoning: true` and `cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }`.
- **Static Fallback Models (`DEFAULT_MODELS`)**:
  - `deepseek-v4.1-flash` (1,048,000 context, text+image)
  - `deepseek-v4-flash` (1,048,000 context, text)
  - `deepseek-v4-flash-vision-exp` (1,048,000 context, text+image)
  - `deepseek-v4-pro` (1,048,000 context, text)
  - `claude-haiku-5.5` (1,000,000 context, text+image)
  - `glm-5.3-flash` (1,048,000 context, text+image)
  - `glm-5.3-flashx` (1,000,000 context, text+image)
  - `gpt-6-luna` (1,050,000 context, text+image)
  - `kimi-k2.7-code` (262,000 context, text+image)
  - `mimo-v2.6-flash` (1,048,000 context, text+image)
  - `mimo-v2.6-pro` (1,048,000 context, text+image)

## 4. Source Implementation (`index.ts` & `models.ts`)
```typescript
export const OPENDESIGN_BASE_URL = "https://amr-link.open-design.ai/v1";
export const OPENDESIGN_PROVIDER_ID = "opendesign";
export const OPENDESIGN_PROVIDER_NAME = "OpenDesign";
export const OPENDESIGN_API_KEY_ENV = "OPENDESIGN_API_KEY";

export async function refreshOpenDesignModels(
	context: RefreshModelsContext,
	baseUrl: string = OPENDESIGN_BASE_URL,
): Promise<ProviderModelConfig[]> {
	if (!context.allowNetwork) return DEFAULT_MODELS;

	const apiKey =
		context.credential?.type === "api_key" && context.credential.key
			? context.credential.key
			: process.env[OPENDESIGN_API_KEY_ENV];

	if (!apiKey) return DEFAULT_MODELS;

	try {
		const res = await fetch(`${baseUrl}/models`, {
			method: "GET",
			headers: {
				Authorization: `Bearer ${apiKey}`,
			},
			signal: context.signal,
		});

		if (!res.ok) return DEFAULT_MODELS;

		const json: unknown = await res.json();
		const models = parseOpenDesignModels(json);
		return models.length > 0 ? models : DEFAULT_MODELS;
	} catch {
		return DEFAULT_MODELS;
	}
}

export function createOpenDesignProviderConfig(baseUrl: string = OPENDESIGN_BASE_URL): ProviderConfig {
	return {
		name: OPENDESIGN_PROVIDER_NAME,
		baseUrl,
		apiKey: `$${OPENDESIGN_API_KEY_ENV}`,
		api: "openai-completions",
		models: DEFAULT_MODELS,
		refreshModels: (context) => refreshOpenDesignModels(context, baseUrl),
	};
}

export default function (pi: ExtensionAPI): void {
	pi.registerProvider(OPENDESIGN_PROVIDER_ID, createOpenDesignProviderConfig());
}
```
