# llama.cpp (`llama.cpp`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `llama.cpp`
- **Name**: `llama.cpp`
- **API Protocols**:
  - Chat: `openai-completions` (`POST <serverUrl>/v1/chat/completions`)
  - Native Decision Classifier: `typesafe-system-one` (`POST <serverUrl>/v1/systemone`)
  - Fallback Next-Token Classifier: `llama-cpp-classify` (`POST <serverUrl>/v1/completions` with logprobs)
- **Default Server URL**: `http://127.0.0.1:8080` (`http://127.0.0.1:8080/v1`)
- **Source Files**:
  - `packages/coding-agent/src/extensions/llama/provider.ts`
  - `packages/coding-agent/src/extensions/llama/client.ts`
  - `packages/ai/src/api/llama-cpp-classify.ts`

## 2. Authentication & Discovery
- **Environment Variables**: `LLAMA_BASE_URL` (server URL), `LLAMA_API_KEY` (optional API key, defaults to `"local"`).
- **Dynamic Model Discovery (`LlamaClient`)**:
  - Queries `GET <serverUrl>/models` (or `/v1/models`) and `GET <serverUrl>/props`.
  - Inspects `status.value` (`"loaded"`, `"sleeping"`, or `"unloaded"` with `models_autoload === true`).
  - Inspects `props.chat_template` for `"enable_thinking"` to enable `thinkingFormat: "qwen-chat-template"`.
  - Inspects `architecture.output_modalities` for `"decisions"` (llama.cpp >= 0.6.0) to register native `typesafe-system-one` classifier models.

## 3. Source Implementation (`packages/coding-agent/src/extensions/llama/provider.ts`)
```typescript
function toPiModel(
	model: LlamaModelInfo,
	serverUrl: string,
	props?: LlamaServerProps,
	cachedContextWindow?: number,
): Model<"openai-completions"> {
	const contextWindow = contextWindowOf(model, cachedContextWindow);
	const reasoning = props?.chat_template?.includes("enable_thinking") === true;
	return {
		id: model.id,
		name: model.id,
		api: "openai-completions",
		provider: LLAMA_PROVIDER_ID,
		baseUrl: llamaInferenceUrl(serverUrl),
		reasoning,
		...(reasoning && {
			thinkingLevelMap: { off: "off", minimal: null, low: null, medium: "medium", high: null, xhigh: null },
		}),
		input: model.architecture?.input_modalities?.includes("image") ? ["text", "image"] : ["text"],
		cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
		contextWindow,
		maxTokens: contextWindow,
		compat: {
			supportsStore: false,
			supportsDeveloperRole: false,
			supportsReasoningEffort: false,
			supportsUsageInStreaming: true,
			supportsStrictMode: false,
			maxTokensField: "max_tokens",
			...(reasoning && { thinkingFormat: "qwen-chat-template" }),
		},
	};
}
```
