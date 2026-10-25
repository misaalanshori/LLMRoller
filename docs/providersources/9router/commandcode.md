# Command Code (`commandcode`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `commandcode`
- **Priority**: `100`
- **Primary Alias**: `commandcode`
- **Additional Aliases**: `["cmc"]`
- **UI Alias**: `cmc`
- **Display Name**: Command Code
- **Category**: `apikey`
- **Website**: `https://commandcode.ai`
- **API Key URL**: `https://commandcode.ai/studio` (or `~/.commandcode/auth.json`, keys starting with `user_...`)
- **Executor**: `CommandCodeExecutor` (`open-sse/executors/commandcode.js`)
- **Source Files**:
  - `open-sse/providers/registry/commandcode.js`
  - `open-sse/executors/commandcode.js`
  - `open-sse/translator/request/openai-to-commandcode.js`
  - `open-sse/translator/response/commandcode-to-openai.js`

---

## 2. Endpoints & Transport
- **Endpoint**: `POST https://api.commandcode.ai/alpha/generate`
- **Wire Format**: `"commandcode"` (`forceStream: true`)
- **Upstream Stream Format**: AI SDK v5 NDJSON (one JSON object per line without `data:` prefix), wrapped and converted in real time to OpenAI `text/event-stream` SSE chunks by `CommandCodeExecutor`.

---

## 3. Authentication & Headers
- **Authorization Header**: `Authorization: Bearer <user_xxx>`
- **Static Registry Headers**:
  - `x-command-code-version`: `"0.25.7"`
  - `x-cli-environment`: `"cli"`
- **Per-Request Dynamic Headers (`CommandCodeExecutor.buildHeaders`)**:
  - `x-session-id`: `<randomUUID()>`
  - `Content-Type`: `"application/json"`
  - `Accept`: `"text/event-stream"`

---

## 4. Request & Response Translation

### Request Envelope (`openaiToCommandCodeRequest`)
```json
{
  "threadId": "<uuid>",
  "memory": "",
  "config": {
    "workingDir": "<process.cwd()>",
    "date": "YYYY-MM-DD",
    "environment": "<process.platform>",
    "structure": [],
    "isGitRepo": false,
    "currentBranch": "",
    "mainBranch": "",
    "gitStatus": "",
    "recentCommits": []
  },
  "params": {
    "model": "<model>",
    "messages": [
      {
        "role": "user",
        "content": [{ "type": "text", "text": "..." }]
      }
    ],
    "stream": true,
    "max_tokens": 16384,
    "temperature": 0.3,
    "system": "<concatenated system prompts>",
    "tools": [
      {
        "name": "<name>",
        "description": "<description>",
        "input_schema": { "type": "object", "properties": {} }
      }
    ]
  }
}
```
- **Message Block Rules**:
  - `system` messages are extracted from `messages[]` and joined into `params.system`.
  - Every `messages[*].content` MUST be an array of blocks (never a string).
  - Assistant tool calls are encoded as `{ type: "tool-call", toolCallId, toolName, input }` accompanied by a leading `{ type: "reasoning", text: rc || " " }` block.
  - Tool results (`role: "tool"`) are encoded as `{ role: "tool", content: [{ type: "tool-result", toolCallId, toolName, output: { type: "text", value } }] }`.

### Response Stream Translation (`commandCodeToOpenAIResponse` & `inspectAndWrapCommandCodeResponse`)
- Inspects the first events of the NDJSON stream before returning the `Response` so mid-stream `{ "type": "error" }` events are converted into proper HTTP 4xx/5xx error responses (with up to 2 automatic retries on 502/503/504).
- Maps NDJSON event types (`text-delta`, `reasoning-delta`, `tool-input-start`, `tool-input-delta`, `tool-call`, `finish-step`, `finish`) to OpenAI `chat.completion.chunk` SSE events.

---

## 5. Supported Models
| Model ID | Display Name |
|---|---|
| `deepseek/deepseek-v4-pro` | DeepSeek V4 Pro |
| `deepseek/deepseek-v4-flash` | DeepSeek V4 Flash |
| `moonshotai/Kimi-K2.7-Code` | Kimi K2.7 Code |
| `moonshotai/Kimi-K2.7-Code-Highspeed` | Kimi K2.7 Code HighSpeed |
| `moonshotai/Kimi-K2.6` | Kimi K2.6 |
| `moonshotai/Kimi-K2.5` | Kimi K2.5 |
| `zai-org/GLM-5.2` | GLM 5.2 |
| `zai-org/GLM-5.2-Fast` | GLM 5.2 Fast |
| `zai-org/GLM-5.1` | GLM 5.1 |
| `zai-org/GLM-5` | GLM 5 |
| `MiniMaxAI/MiniMax-M3` | MiniMax M3 |
| `MiniMaxAI/MiniMax-M2.7` | MiniMax M2.7 |
| `MiniMaxAI/MiniMax-M2.5` | MiniMax M2.5 |
| `xiaomi/mimo-v2.5-pro` | MiMo V2.5 Pro |
| `xiaomi/mimo-v2.5` | MiMo V2.5 |
| `Qwen/Qwen3.6-Max-Preview` | Qwen 3.6 Max Preview |
| `Qwen/Qwen3.6-Plus` | Qwen 3.6 Plus |
| `Qwen/Qwen3.7-Max` | Qwen 3.7 Max |
| `Qwen/Qwen3.7-Plus` | Qwen 3.7 Plus |
| `stepfun/Step-3.7-Flash` | Step 3.7 Flash |
| `stepfun/Step-3.5-Flash` | Step 3.5 Flash |
| `nvidia/nemotron-3-ultra-550b-a55b` | Nemotron 3 Ultra |

---

## 6. Verbatim Source Code Snippets

### `open-sse/providers/registry/commandcode.js`
```javascript
export default {
  id: "commandcode",
  priority: 100,
  alias: "commandcode",
  aliases: [
    "cmc",
  ],
  uiAlias: "cmc",
  display: {
    name: "Command Code",
    icon: "smart_toy",
    color: "#000000",
    textIcon: "CC",
    website: "https://commandcode.ai",
    notice: {
      text: "Use your CommandCode CLI API key (starts with user_...) from ~/.commandcode/auth.json or commandcode.ai/studio.",
      apiKeyUrl: "https://commandcode.ai/studio",
    },
  },
  category: "apikey",
  transport: {
    baseUrl: "https://api.commandcode.ai/alpha/generate",
    format: "commandcode",
    forceStream: true,
    headers: {
      "x-command-code-version": "0.25.7",
      "x-cli-environment": "cli",
    },
  },
  features: {
    usage: true,
    usageApikey: true,
  },
};
```

### `open-sse/translator/request/openai-to-commandcode.js` (Excerpt)
```javascript
export function openaiToCommandCodeRequest(model, body, stream /* , credentials */) {
  const { messages, system } = convertMessages(body.messages);
  const params = {
    model,
    messages,
    stream: stream !== false,
    max_tokens: body.max_tokens ?? body.max_output_tokens ?? DEFAULT_MAX_TOKENS,
    temperature: body.temperature ?? 0.3,
  };

  if (system) params.system = system;

  const tools = convertTools(body.tools);
  if (tools) params.tools = tools;
  if (body.top_p != null) params.top_p = body.top_p;

  const today = new Date().toISOString().slice(0, 10);

  return {
    threadId: randomUUID(),
    memory: "",
    config: {
      workingDir: process.cwd(),
      date: today,
      environment: process.platform,
      structure: [],
      isGitRepo: false,
      currentBranch: "",
      mainBranch: "",
      gitStatus: "",
      recentCommits: [],
    },
    params,
  };
}
```
