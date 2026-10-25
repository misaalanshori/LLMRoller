# Google Generative AI (`google`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `google`
- **API Protocol**: `google-generative-ai` (`packages/ai/src/api/google-generative-ai.ts` & `packages/ai/src/api/google-shared.ts`)
- **Base URL**: `https://generativelanguage.googleapis.com/v1beta`
- **Source Files**:
  - `packages/ai/src/providers/google.ts`
  - `packages/ai/src/api/google-generative-ai.ts`
  - `packages/ai/src/api/google-shared.ts`

## 2. Authentication & Client Initialization
- **Environment Variable**: `GEMINI_API_KEY`
- **Headers**: Injects `x-goog-api-key` via `@google/genai` SDK (`GoogleGenAI` client with `apiVersion: "v1beta"`), plus any custom `model.headers` or `options.headers` via `httpOptions.headers`.

## 3. Thinking / Reasoning Configuration & Thought Signatures
- **Gemini 3 / 4 & Gemma 4 Models**: Uses `thinkingConfig.thinkingLevel` (`MINIMAL`, `LOW`, `MEDIUM`, `HIGH`).
- **Gemini 2.5 Models**: Uses numeric `thinkingConfig.thinkingBudget` (`-1` dynamic default, or explicit token budget).
- **Thought Signatures (`thoughtSignature`)**: Preserves base64 `thoughtSignature` across streaming chunks (`retainThoughtSignature`) and replays them on assistant `thought` and `functionCall` parts during multi-turn tool calls. For unsigned tool calls in Gemini 3/4, injects dummy signature `skip_thought_signature_validator` (`btoa("skip_thought_signature_validator")`).

## 4. Source Implementation (`packages/ai/src/providers/google.ts` & `packages/ai/src/api/google-generative-ai.ts`)
```typescript
export const googleProvider: ProviderDefinition = {
	id: "google",
	models: [
		defineModel({
			id: "gemini-2.5-pro",
			name: "Gemini 2.5 Pro",
			api: "google-generative-ai",
			provider: "google",
			baseUrl: "https://generativelanguage.googleapis.com/v1beta",
			reasoning: true,
			input: ["text", "image"],
			cost: { input: 1.25, output: 10, cacheRead: 0.31, cacheWrite: 0 },
			contextWindow: 1048576,
			maxTokens: 65536,
		}),
		// ... gemini-2.5-flash, gemini-3.1-pro-preview, gemini-3-flash-preview, gemma-4-31b-it, etc.
	],
};

function createClient(
	model: Model<"google-generative-ai">,
	apiKey?: string,
	optionsHeaders?: Record<string, string>,
): { client: GoogleGenAI; clientOptions: GoogleGenAIOptions } {
	const httpOptions: { baseUrl?: string; apiVersion?: string; headers?: Record<string, string> } = {};
	if (model.baseUrl) {
		httpOptions.baseUrl = model.baseUrl;
		httpOptions.apiVersion = "";
	}
	if (model.headers || optionsHeaders) {
		httpOptions.headers = { ...model.headers, ...optionsHeaders };
	}
	const clientOptions: GoogleGenAIOptions = {
		apiKey: apiKey ?? "",
		httpOptions: Object.keys(httpOptions).length > 0 ? httpOptions : undefined,
	};
	return { client: new GoogleGenAI(clientOptions), clientOptions };
}
```
