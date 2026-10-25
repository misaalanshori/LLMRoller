# Vertex Partner (`vertex-partner`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `vertex-partner`
- **Display Name**: `Vertex Partner`
- **Alias**: `vxp`
- **Category**: `apikey`
- **Service Kinds**: `["llm"]`
- **Executor**: `VertexExecutor` (`open-sse/executors/vertex.js`)
- **Website**: `https://cloud.google.com/vertex-ai/generative-ai/docs/partner-models/use-partner-models`

---

## 2. Endpoints & Authentication

| Parameter | Value |
|---|---|
| **Endpoint** | `POST https://aiplatform.googleapis.com/v1/projects/{projectId}/locations/global/endpoints/openapi/chat/completions` |
| **Authentication** | Service Account JSON / ADC JSON (`Authorization: Bearer <accessToken>`) or raw Vertex API key (`?key={rawKey}` + automatic `projectId` probe via `resolveProjectId(apiKey)`) |
| **Wire Format** | `"openai"` |

---

## 3. URL & Project ID Resolution (`open-sse/executors/vertex.js`)

```javascript
if (this.provider === "vertex-partner") {
  // Partner models require project_id in path regardless of auth method
  if (!projectId) throw new Error("Vertex partner models require a project_id. Add it in providerSpecificData or use Service Account JSON.");
  const url = `https://aiplatform.googleapis.com/v1/projects/${projectId}/locations/global/endpoints/openapi/chat/completions`;
  return rawKey ? `${url}?key=${rawKey}` : url;
}
```

---

## 4. Supported Models (`open-sse/providers/registry/vertex-partner.js`)

| Model ID | Display Name |
|---|---|
| `deepseek-ai/deepseek-v3.2-maas` | DeepSeek V3.2 (Vertex) |
| `qwen/qwen3-next-80b-a3b-thinking-maas` | Qwen3 Next 80B Thinking (Vertex) |
| `qwen/qwen3-next-80b-a3b-instruct-maas` | Qwen3 Next 80B Instruct (Vertex) |
| `zai-org/glm-5-maas` | GLM-5 (Vertex) |
