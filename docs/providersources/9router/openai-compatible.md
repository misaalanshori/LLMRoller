# OpenAI-Compatible Dynamic Provider (`openai-compatible`) Specification (`9router`)

## 1. Overview & Identification

- **Provider Prefix**: `openai-compatible-*` (`OPENAI_COMPATIBLE_PREFIX = "openai-compatible-"`)
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Embedding Handler**: `open-sse/handlers/embeddingProviders/openaiCompatNode.js`
- **Service Kinds**: `["llm", "embedding"]`

---

## 2. Dynamic Endpoint & API Type Resolution (`open-sse/executors/default.js`)

- **Default Base URL**: `https://api.openai.com/v1` (overridden by `credentials.providerSpecificData.baseUrl`)
- **Chat / Responses Routing**:
  - If `resolveOpenAICompatibleApiType(provider, credentials) === "responses"`, sends `POST ${baseUrl}/responses` (OpenAI Responses API format).
  - Otherwise sends `POST ${baseUrl}/chat/completions` (OpenAI Chat Completions format).
- **Authentication**: `Authorization: Bearer <apiKey>`

---

## 3. Special Transformations (`open-sse/executors/default.js`)

1. **Structured Outputs (`json_schema`) Fallback (`applyJsonSchemaFallback`)**:
   Converts `response_format: { type: "json_schema", json_schema: { schema } }` into a system prompt instruction with `response_format: { type: "json_object" }` for OpenAI-compatible gateways that lack native `json_schema` support.
2. **Nous Research Attribution (`applyNousAttribution`)**:
   When an `openai-compatible-*` node targets `https://inference-api.nousresearch.com/v1/chat/completions`, automatically injects required attribution tags into `body.tags`.

```javascript
if (this.provider?.startsWith?.("openai-compatible-")) {
  const baseUrl = credentials?.providerSpecificData?.baseUrl || OPENAI_COMPAT_BASE;
  const normalized = baseUrl.replace(/\/$/, "");
  const path = resolveOpenAICompatibleApiType(this.provider, credentials) === "responses" ? "/responses" : "/chat/completions";
  return `${normalized}${path}`;
}
```
