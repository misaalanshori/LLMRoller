# Faux (`faux`) Scriptable Test Provider Specification (`pi`)

## 1. Overview & Identification

- **Provider ID**: `faux` (configurable via `RegisterFauxProviderOptions.provider`)
- **API ID**: `faux` (configurable via `RegisterFauxProviderOptions.api`)
- **Provider Module**: `packages/ai/src/providers/faux.ts`
- **Purpose**: In-memory deterministic/scriptable LLM provider (`createFauxProvider`, `registerFauxProvider`) supporting token-chunked streaming, tool call argument streaming (`ToolCallArgumentsStream`), thinking blocks, and deferred response polling (`fetchDeferred` / `cancelDeferred`).

---

## 2. Implementation (`packages/ai/src/providers/faux.ts`)

```typescript
const DEFAULT_API = "faux";
const DEFAULT_PROVIDER = "faux";
const DEFAULT_MODEL_ID = "faux-1";
const DEFAULT_MODEL_NAME = "Faux Model";
const DEFAULT_BASE_URL = "http://localhost:0";

export function fauxAssistantMessage(
	content: string | FauxContentBlock | FauxContentBlock[],
	options: {
		stopReason?: AssistantMessage["stopReason"];
		deferred?: DeferredHandle;
		errorMessage?: string;
		responseId?: string;
		timestamp?: number;
	} = {},
): AssistantMessage {
	return {
		role: "assistant",
		content: normalizeFauxAssistantContent(content),
		api: DEFAULT_API,
		provider: DEFAULT_PROVIDER,
		model: DEFAULT_MODEL_ID,
		usage: DEFAULT_USAGE,
		stopReason: options.stopReason ?? "stop",
		...(options.deferred === undefined ? {} : { deferred: options.deferred }),
		...(options.errorMessage === undefined ? {} : { errorMessage: options.errorMessage }),
		...(options.responseId === undefined ? {} : { responseId: options.responseId }),
		timestamp: options.timestamp ?? Date.now(),
	};
}
```
