# Google Vertex AI (`google-vertex`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `google-vertex`
- **API Protocol**: `google-vertex` (`packages/ai/src/api/google-vertex.ts` & `packages/ai/src/api/google-shared.ts`)
- **Base URL**: `https://{location}-aiplatform.googleapis.com` (via `@google/genai` Vertex mode with `apiVersion: "v1"`)
- **Source Files**:
  - `packages/ai/src/providers/google-vertex.ts`
  - `packages/ai/src/api/google-vertex.ts`

## 2. Authentication & Resolution
Supports two modes in `packages/ai/src/api/google-vertex.ts`:
1. **API Key Express Mode**: Resolved from `options.apiKey` or `GOOGLE_CLOUD_API_KEY` (`new GoogleGenAI({ vertexai: true, apiKey, httpOptions })`).
2. **Project + Location (ADC)**:
   - Project resolved from `options.project`, `GOOGLE_CLOUD_PROJECT`, or `GCLOUD_PROJECT`.
   - Location resolved from `options.location` or `GOOGLE_CLOUD_LOCATION` (defaults to `us-central1` / `global`).
   - Checks `GOOGLE_APPLICATION_CREDENTIALS` or `~/.config/gcloud/application_default_credentials.json`.

## 3. Source Implementation (`packages/ai/src/api/google-vertex.ts`)
```typescript
function createClient(
	model: Model<"google-vertex">,
	project?: string,
	location?: string,
	apiKey?: string,
	optionsHeaders?: Record<string, string>,
): { client: GoogleGenAI; clientOptions: GoogleGenAIOptions } {
	const httpOptions: { headers?: Record<string, string>; apiVersion?: string } = {};

	if (model.headers || optionsHeaders) {
		httpOptions.headers = { ...model.headers, ...optionsHeaders };
	}

	const hasHttpOptions = Object.keys(httpOptions).length > 0;

	if (apiKey) {
		const clientOptions: GoogleGenAIOptions = {
			vertexai: true,
			apiKey,
			httpOptions: hasHttpOptions ? httpOptions : undefined,
		};
		return { client: new GoogleGenAI(clientOptions), clientOptions };
	}

	httpOptions.apiVersion = "v1";

	const clientOptions: GoogleGenAIOptions = {
		vertexai: true,
		project,
		location,
		httpOptions,
	};

	return { client: new GoogleGenAI(clientOptions), clientOptions };
}
```
