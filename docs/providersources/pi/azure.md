# Azure OpenAI (`azure`) Provider Specification (`pi`)

## 1. Overview & Identification

- **Provider ID**: `azure`
- **Display Name**: `Azure`
- **Provider Module**: `packages/ai/src/providers/azure.ts`
- **API Implementations**:
  - `packages/ai/src/api/azure-openai-responses.ts` (`azure-openai-responses`)
  - `packages/ai/src/api/azure-openai-config.ts`
  - `packages/ai/src/api/openai-completions.ts` (`openai-completions`)
- **Models Catalog**: `packages/ai/src/providers/azure.models.ts` (`AZURE_MODELS`)

---

## 2. Endpoints, Base URL Normalization & Deployment Mapping (`azure-openai-config.ts`)

| Parameter | Value |
|---|---|
| **Default Resource Base URL** | `https://${resourceName}.openai.azure.com/openai/v1` |
| **Default API Version** | `v1` (overridable via `azureApiVersion` or `AZURE_OPENAI_API_VERSION`) |
| **Environment Variables** | `AZURE_OPENAI_API_KEY`, `AZURE_OPENAI_BASE_URL`, `AZURE_OPENAI_RESOURCE_NAME`, `AZURE_OPENAI_API_VERSION`, `AZURE_OPENAI_DEPLOYMENT_NAME_MAP` |

- **Base URL Normalization**: Automatically rewrites bare `*.openai.azure.com`, `*.cognitiveservices.azure.com`, or `*.ai.azure.com` paths (`/`, `/openai`, `/openai/v1/responses`) to `/openai/v1`.
- **Deployment Name Mapping**: Parses comma-separated `modelId=deploymentName` pairs from `AZURE_OPENAI_DEPLOYMENT_NAME_MAP` (or `options.azureDeploymentName`) and rewrites `payload.model = deploymentName` before dispatch.

---

## 3. Implementation (`packages/ai/src/providers/azure.ts`)

```typescript
export function azureProvider(): Provider<"azure-openai-responses" | "openai-completions"> {
	return createProvider({
		id: "azure",
		name: "Azure",
		auth: { apiKey: envApiKeyAuth("Azure OpenAI API key", ["AZURE_OPENAI_API_KEY"]) },
		models: Object.values(AZURE_MODELS),
		api: {
			"azure-openai-responses": azureOpenAIResponsesApi(),
			"openai-completions": azureStreams(openAICompletionsApi()),
		},
	});
}
```
