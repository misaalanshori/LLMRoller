# Alibaba Model Studio (`alibaba`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `alibaba`
- **Package Entrypoints**:
  - `@opencode/ai/providers/alibaba` (`packages/ai/src/providers/alibaba.ts`)
  - `@opencode/ai/providers/alibaba/chat` (`packages/ai/src/providers/alibaba/chat.ts`)
  - `@opencode/ai/providers/alibaba/messages` (`packages/ai/src/providers/alibaba/messages.ts`)
  - `@opencode/ai/providers/alibaba/responses` (`packages/ai/src/providers/alibaba/responses.ts`)
- **Protocols**:
  - `packages/ai/src/protocols/alibaba-chat.ts`
  - `packages/ai/src/protocols/alibaba-messages.ts`
  - `packages/ai/src/protocols/alibaba-responses.ts`
  - `packages/ai/src/protocols/alibaba-images.ts`
- **Catalog Host Aliases (`packages/core/src/aisdk-native.ts`)**: `alibaba`, `alibaba-cn`, `alibaba-coding-plan`, `alibaba-coding-plan-cn`, `alibaba-token-plan`, `alibaba-token-plan-cn`

---

## 2. Regional Host Resolution & Endpoints (`packages/ai/src/providers/alibaba.ts`)

| Region | Default Host | Workspace-Scoped Host (`workspaceID` set) |
|---|---|---|
| `ap-southeast-1` | `dashscope-intl.aliyuncs.com` | `${workspaceID}.ap-southeast-1.maas.aliyuncs.com` |
| `cn-beijing` | `dashscope.aliyuncs.com` | `${workspaceID}.cn-beijing.maas.aliyuncs.com` |
| `cn-hongkong` | `cn-hongkong.dashscope.aliyuncs.com` | `${workspaceID}.cn-hongkong.maas.aliyuncs.com` |
| `us-east-1` | `dashscope-us.aliyuncs.com` | `${workspaceID}.us-east-1.maas.aliyuncs.com` |
| `eu-central-1` / `ap-northeast-1` / custom | Requires `workspaceID` or `baseURL` | `${workspaceID}.${region}.maas.aliyuncs.com` |

| Route | Base URL | Path | Headers & Framing |
|---|---|---|---|
| **Chat (`alibaba-chat`)** | `https://${host}/compatible-mode/v1` | `POST /chat/completions` | `Authorization: Bearer <key>`, SSE |
| **Messages (`alibaba-messages`)** | `https://${host}/apps/anthropic/v1` | `POST /messages` | `Authorization: Bearer <key>`, `anthropic-version: 2023-06-01`, SSE |
| **Responses (`alibaba-responses`)** | `https://${host}/compatible-mode/v1` | `POST /responses` | `Authorization: Bearer <key>`, SSE |
| **Images (`alibaba-images`)** | `https://${host}/api/v1` | `POST /services/aigc/multimodal-generation/generation` | `Authorization: Bearer <key>`, JSON |

- **Environment Variable Fallbacks**: `DASHSCOPE_API_KEY`, `ALIBABA_API_KEY`

---

## 3. Provider & Route Implementation (`packages/ai/src/providers/alibaba.ts`)

```typescript
const hosts = new Map<string, string>([
  ["ap-southeast-1", "dashscope-intl.aliyuncs.com"],
  ["cn-beijing", "dashscope.aliyuncs.com"],
  ["cn-hongkong", "cn-hongkong.dashscope.aliyuncs.com"],
  ["us-east-1", "dashscope-us.aliyuncs.com"],
])
const chatRoute = Route.make({
  id: "alibaba-chat",
  provider: id,
  providerMetadataKey: "alibaba",
  protocol: AlibabaChat.protocol,
  endpoint: Endpoint.path("/chat/completions"),
  framing: Framing.sse,
})
const messagesRoute = Route.make({
  id: "alibaba-messages",
  provider: id,
  providerMetadataKey: "alibaba",
  protocol: AlibabaMessages.protocol,
  endpoint: Endpoint.path("/messages"),
  framing: Framing.sse,
  headers: () => ({ "anthropic-version": "2023-06-01" }),
})
const responsesRoute = Route.make({
  id: "alibaba-responses",
  provider: id,
  providerMetadataKey: "alibaba",
  protocol: AlibabaResponses.protocol,
  endpoint: Endpoint.path("/responses"),
  framing: Framing.sse,
})

export const configure = (input: Config) => {
  const { apiKey: _key, auth: _auth, region, workspaceID, baseURL, ...rest } = input
  const host =
    region === undefined
      ? undefined
      : workspaceID === undefined
        ? hosts.get(region)
        : `${workspaceID}.${region}.maas.aliyuncs.com`
  const opts = { ...rest, auth: AuthOptions.bearer(input, ["DASHSCOPE_API_KEY", "ALIBABA_API_KEY"]) }
  const common = { ...opts, endpoint: { baseURL: baseURL ?? `https://${host}/compatible-mode/v1` } }
  const chat = (id: string | ModelID) =>
    chatRoute.with(common).model<ChatOptionsInput>({ id, compatibility: AlibabaChat.compatibility })
  const messages = (id: string | ModelID) =>
    messagesRoute
      .with({
        ...opts,
        endpoint: { baseURL: baseURL ?? `https://${host}/apps/anthropic/v1` },
      })
      .model<MessagesOptionsInput>({ id, compatibility: { requireSignature: false } })
  const responses = (id: string | ModelID) => responsesRoute.with(common).model<ResponsesOptionsInput>({ id })
  const media = MediaRoute.deployment({ ...input, baseURL: baseURL ?? `https://${host}/api/v1` }, opts.auth)
  const image = (id: string | ModelID) => AlibabaImages.model({ ...media, id })
  return { id, model: chat, chat, messages, responses, image, configure }
}
```

---

## 4. Protocol Details

### 4.1 `alibaba-chat` (`packages/ai/src/protocols/alibaba-chat.ts`)
- Compatibility: `{ maxTokensField: "max_completion_tokens", supportsStrictMode: false, reasoningField: "reasoning_content", zaiToolStream: false }`.
- Supports `enable_thinking`, `thinking_budget` (automatically clamped below `max_completion_tokens` via `ProviderShared.fitThinkingBudget`), `preserve_thinking`, `clear_thinking`, `thinking: { type: "adaptive" | "disabled" }`, `tool_stream`, `parallel_tool_calls`, `repetition_penalty`, `top_k`, `response_format`, `enable_search`, and `search_options` (`forced_search`, `search_strategy: "turbo" | "max" | "agent" | "agent_max"`, `enable_search_extension`).

### 4.2 `alibaba-messages` (`packages/ai/src/protocols/alibaba-messages.ts`)
- Extends `AnthropicMessages.protocol` but allows `thinking: { type: "enabled" | "disabled" }` without requiring Anthropic's mandatory `budget_tokens`, while fitting `budget_tokens` under `max_tokens` when provided.

### 4.3 `alibaba-responses` (`packages/ai/src/protocols/alibaba-responses.ts`)
- Extends `OpenResponses.protocol` with native hosted tools (`web_search`, `web_extractor`, `code_interpreter`), `enable_thinking`, `previous_response_id`, `conversation`, and rewrites function `tool_choice` into `{ type: "allowed_tools", mode: "required", tools: [choice] }`.

### 4.4 `alibaba-images` (`packages/ai/src/protocols/alibaba-images.ts`)
- Sends `POST /services/aigc/multimodal-generation/generation` with `{ model, input: { messages: [{ role: "user", content: [...images, { text: prompt }] }] }, parameters: { size: "WIDTH*HEIGHT", n, negative_prompt, prompt_extend, thinking_mode, enable_sequential, ... } }`.
- Extracts image URLs from `output.choices[].message.content[].image` with a 24-hour retention window.
