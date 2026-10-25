# Perplexity Agent (`perplexity-agent`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `perplexity-agent`
- **Priority**: `181`
- **Primary Alias**: `perplexity-agent`
- **Additional Aliases**: `["pplx-agent", "pplx-responses"]`
- **UI Alias**: `pa`
- **Display Name**: Perplexity Agent
- **Category**: `apikey`
- **Auth Type**: `apikey`
- **Service Kinds**: `["llm", "webSearch"]`
- **Website**: `https://www.perplexity.ai`
- **API Key URL**: `https://www.perplexity.ai/settings/api`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source File**: `open-sse/providers/registry/perplexity-agent.js`

---

## 2. Endpoints & Transport
- **Responses API Endpoint**: `POST https://api.perplexity.ai/v1/responses` (`format: "openai-responses"`)
- **Validation / Models Endpoint**: `GET https://api.perplexity.ai/v1/models` (`modelsFetcher: { url: "https://api.perplexity.ai/v1/models", type: "openai" }`, `passthroughModels: true`)
- **Web Search via Chat (`searchViaChat`)**: `POST https://api.perplexity.ai/v1/responses` (`defaultModel: "perplexity/sonar"`)
- **Authentication**: `Authorization: Bearer <apiKey>`

---

## 3. Supported Models
| Model ID | Display Name |
|---|---|
| `perplexity/sonar` | Perplexity Sonar |
| `openai/gpt-5.5` | GPT-5.5 |
| `openai/gpt-5.4` | GPT-5.4 |
| `openai/gpt-5.4-mini` | GPT-5.4 Mini |
| `anthropic/claude-sonnet-4-6` | Claude Sonnet 4.6 |
| `anthropic/claude-opus-4-8` | Claude Opus 4.8 |
| `google/gemini-3.1-pro-preview` | Gemini 3.1 Pro |
| `xai/grok-4.20-reasoning` | Grok 4.20 Reasoning |
| `perplexity/glm-5.2` | GLM 5.2 |
| `perplexity/kimi-k2.7-code` | Kimi K2.7 Code |
| `nvidia/nemotron-3-super-120b-a12b` | Nemotron 3 Super 120B |

---

## 4. Verbatim Source Code

### `open-sse/providers/registry/perplexity-agent.js`
```javascript
export default {
  id: "perplexity-agent",
  priority: 181,
  alias: "perplexity-agent",
  aliases: [
    "pplx-agent",
    "pplx-responses",
  ],
  uiAlias: "pa",
  display: {
    name: "Perplexity Agent",
    icon: "travel_explore",
    color: "#20808D",
    textIcon: "PA",
    website: "https://www.perplexity.ai",
    notice: {
      text: "Perplexity Agent API exposes GPT, Claude, Gemini, Grok, GLM, Kimi, and Sonar models through one OpenAI-compatible Responses API.",
      apiKeyUrl: "https://www.perplexity.ai/settings/api",
    },
  },
  category: "apikey",
  authType: "apikey",
  transport: {
    baseUrl: "https://api.perplexity.ai/v1/responses",
    validateUrl: "https://api.perplexity.ai/v1/models",
    format: "openai-responses",
  },
  models: [
    { id: "perplexity/sonar", name: "Perplexity Sonar" },
    { id: "openai/gpt-5.5", name: "GPT-5.5" },
    { id: "openai/gpt-5.4", name: "GPT-5.4" },
    { id: "openai/gpt-5.4-mini", name: "GPT-5.4 Mini" },
    { id: "anthropic/claude-sonnet-4-6", name: "Claude Sonnet 4.6" },
    { id: "anthropic/claude-opus-4-8", name: "Claude Opus 4.8" },
    { id: "google/gemini-3.1-pro-preview", name: "Gemini 3.1 Pro" },
    { id: "xai/grok-4.20-reasoning", name: "Grok 4.20 Reasoning" },
    { id: "perplexity/glm-5.2", name: "GLM 5.2" },
    { id: "perplexity/kimi-k2.7-code", name: "Kimi K2.7 Code" },
    { id: "nvidia/nemotron-3-super-120b-a12b", name: "Nemotron 3 Super 120B" },
  ],
  serviceKinds: ["llm", "webSearch"],
  searchViaChat: {
    defaultModel: "perplexity/sonar",
    endpoint: "https://api.perplexity.ai/v1/responses",
    pricingUrl: "https://docs.perplexity.ai/docs/agent-api/models",
  },
  modelsFetcher: { url: "https://api.perplexity.ai/v1/models", type: "openai" },
  passthroughModels: true,
};
```
