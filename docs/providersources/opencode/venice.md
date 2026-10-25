# Venice AI (`venice`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `venice`
- **Package Entrypoint**: `@opencode/ai/providers/venice` (`packages/ai/src/providers/venice.ts`)
- **Protocol**: `packages/ai/src/protocols/venice-chat.ts` (`venice-chat`)

---

## 2. Endpoints, Authentication & Compatibility

| Parameter | Value |
|---|---|
| **Base URL** | `https://api.venice.ai/api/v1` |
| **Chat Completions Path** | `POST /chat/completions` |
| **Authentication** | `Authorization: Bearer <apiKey>` (env fallback `VENICE_API_KEY`) |
| **Compatibility** | `{ maxTokensField: "max_completion_tokens", supportsPromptCacheKey: true }` |

---

## 3. Protocol Implementation (`packages/ai/src/protocols/venice-chat.ts`)

```typescript
return {
  ...body,
  max_completion_tokens: options.maxCompletionTokens ?? options.maxTokens ?? request.generation?.maxTokens,
  reasoning_effort: undefined,
  reasoning:
    options.reasoningEffort === undefined
      ? options.reasoning
      : { ...options.reasoning, effort: options.reasoningEffort },
  venice_parameters: Object.fromEntries(
    Object.entries({
      ...options.veniceParameters,
      includeVeniceSystemPrompt: options.veniceParameters?.includeVeniceSystemPrompt ?? false,
    })
      .filter(([, value]) => value !== undefined)
      .map(([key, value]) => [key.replace(/[A-Z]/g, (letter) => `_${letter.toLowerCase()}`), value]),
  ),
  prompt_cache_retention: options.promptCacheRetention,
  parallel_tool_calls: options.parallelToolCalls,
  min_p: options.minP,
  top_k: request.generation?.topK,
  repetition_penalty: options.repetitionPenalty,
  stop_token_ids: options.stopTokenIds,
  logprobs: options.logprobs,
  top_logprobs: options.topLogprobs,
  max_temp: options.maxTemp,
  min_temp: options.minTemp,
  response_format: options.responseFormat,
  user: options.user,
}
```
- Sets `venice_parameters.include_venice_system_prompt = false` by default.
- Detects the `__ENCRYPTED_REASONING__` marker in `delta.reasoning_content`, stripping encrypted reasoning from visible reasoning deltas while preserving `encryptedReasoningContent`, `messageThoughtSignature`, and per-tool-call `thoughtSignature` in `providerMetadata.venice` for multi-turn replay.
