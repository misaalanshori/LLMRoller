# TokenRouter (`tokenrouter`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `tokenrouter`
- **Display Name**: `TokenRouter`
- **Alias**: `tr`
- **Category**: `apikey`
- **Service Kinds**: `["llm", "embedding", "image"]`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Website**: `https://tokenrouter.com`

---

## 2. Endpoints & Authentication

| Service | Endpoint |
|---|---|
| **Chat Completions** | `POST https://api.tokenrouter.com/v1/chat/completions` |
| **Embeddings** | `POST https://api.tokenrouter.com/v1/embeddings` |
| **Image Generation** | `POST https://api.tokenrouter.com/v1/images/generations` |
| **Models / Validation URL** | `GET https://api.tokenrouter.com/v1/models` (`modelsFetcher`) |
| **Authentication** | `Authorization: Bearer <apiKey>` |
| **Thinking Format** | `"tokenrouter"` (sets `body.reasoning_effort = effort`) |

---

## 3. Configuration (`open-sse/config/providerRegistry.js`)

```javascript
tokenrouter: {
  baseUrl: "https://api.tokenrouter.com/v1/chat/completions",
  modelsUrl: "https://api.tokenrouter.com/v1/models",
  format: "openai",
  authType: "apikey",
  authHeader: "bearer",
  thinkingFormat: "tokenrouter",
  modelsFetcher: "https://api.tokenrouter.com/v1/models",
},
```

### Seed Models (`open-sse/config/providerModels.js`, `embeddingRegistry.js`, `imageRegistry.js`)
- **LLM**: `openai/gpt-5.4`, `openai/gpt-5-mini`, `openai/gpt-4.1`, `openai/gpt-4.1-mini`, `openai/gpt-4o`, `openai/gpt-4o-mini`, `openai/o3`, `openai/o4-mini`, `anthropic/claude-opus-4.6`, `anthropic/claude-sonnet-4.6`, `anthropic/claude-haiku-4.5`, `google/gemini-3.1-pro-preview`, `google/gemini-3-flash-preview`, `google/gemini-2.5-pro`, `google/gemini-2.5-flash`, `deepseek/deepseek-v3.2`, `deepseek/deepseek-r1`, `x-ai/grok-4`, `x-ai/grok-3`, `meta-llama/llama-4-maverick`, `qwen/qwen3-235b-a22b`, `mistralai/mistral-large-2411`
- **Embedding**: `openai/text-embedding-3-small` (1536), `openai/text-embedding-3-large` (3072), `google/gemini-embedding-001` (768)
- **Image**: `openai/gpt-image-1.5`, `openai/dall-e-3`, `google/imagen-4`
