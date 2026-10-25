# Mistral (`mistral`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `mistral`
- **API Protocol**: `mistral-conversations` (`POST /v1/chat/completions`)
- **Base URL**: `https://api.mistral.ai`
- **Source Files**:
  - `packages/ai/src/providers/mistral.ts`
  - `packages/ai/src/api/mistral-conversations.ts`
  - `packages/ai/src/env-api-keys.ts`

## 2. Authentication & Protocol Quirks
- **Environment Variable**: `MISTRAL_API_KEY`
- **Headers**:
  - `Authorization: Bearer <MISTRAL_API_KEY>`
  - `x-affinity: <sessionId>` (injected when `options.sessionId` is present for KV cache routing).
- **Tool Call ID Normalization (`MISTRAL_TOOL_CALL_ID_LENGTH = 9`)**:
  - Mistral requires strictly 9-character alphanumeric `[a-zA-Z0-9]{9}` tool call IDs. `normalizeMistralToolCallId` hashes or pads/truncates IDs to 9 characters (`deriveMistralToolCallId`).
- **Reasoning Mode**:
  - Models using `prompt_mode: "reasoning"` (`magistral-medium-latest`, `magistral-small-latest`) set `prompt_mode = "reasoning"`.
  - Newer models (`mistral-small-2603`) use `reasoning_effort` (`"none" | "minimal" | "low" | "medium" | "high" | "xhigh"`).
- **Trailing Tool Result Workaround**:
  - When a conversation ends with a tool result followed by a user message, Mistral's API rejects the sequence or hallucinates; handled in `toChatMessages` message normalization.

## 3. Source Implementation (`packages/ai/src/api/mistral-conversations.ts`)
```typescript
const MISTRAL_TOOL_CALL_ID_LENGTH = 9;

function buildChatEndpointUrl(baseUrl: string): string {
	const trimmed = baseUrl.trim().replace(/\/+$/, "");
	if (trimmed.endsWith("/v1/chat/completions")) return trimmed;
	if (trimmed.endsWith("/v1")) return `${trimmed}/chat/completions`;
	return `${trimmed}/v1/chat/completions`;
}

function buildChatPayload(
	model: Model<"mistral-conversations">,
	context: Context,
	messages: MistralChatMessage[],
	options?: MistralConversationsOptions,
): Record<string, unknown> {
	const payload: Record<string, unknown> = {
		model: model.id,
		messages,
		stream: true,
	};
	if (options?.maxTokens !== undefined) payload.max_tokens = options.maxTokens;
	if (options?.temperature !== undefined) payload.temperature = options.temperature;
	if (context.tools && context.tools.length > 0) {
		payload.tools = toFunctionTools(context.tools);
		payload.tool_choice = "auto";
	}
	if (model.reasoning) {
		if (usesReasoningEffort(model.id)) {
			payload.reasoning_effort = mapReasoningEffort(options?.reasoning);
		} else if (options?.reasoning && usesReasoningPromptMode(model.id)) {
			payload.prompt_mode = "reasoning";
		}
	}
	return payload;
}
```
