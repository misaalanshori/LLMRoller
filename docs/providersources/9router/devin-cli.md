# Devin CLI (`devin-cli`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `devin-cli`
- **Primary Alias**: `dv`
- **Additional Aliases**: `["devin"]`
- **UI Alias**: `dv`
- **Hidden**: `true` (commented out in `open-sse/providers/registry/index.js` as temporarily hidden, with full registry and executor implementation in the repository)
- **Display Name**: Devin CLI
- **Category**: `free`
- **Auth Type**: `none` (`noAuth: true` — uses local credentials from `devin auth login` stored at `~/.local/share/devin/credentials.toml`)
- **Website**: `https://devin.ai` (`https://cli.devin.ai`)
- **Executor**: `DevinCliExecutor` (`open-sse/executors/devin-cli.js`)
- **Source Files**:
  - `open-sse/providers/registry/devin-cli.js`
  - `open-sse/executors/devin-cli.js`

---

## 2. Transport & Agent Client Protocol (ACP) over Stdio
- **Virtual Transport URI**: `devin://acp/stdio`
- **Subprocess Execution**:
  - Resolves binary via `CLI_DEVIN_BIN` env var, known install paths (`~/.local/share/devin/bin/devin`, `~/.devin/bin/devin`, `/opt/homebrew/bin/devin`, `%LOCALAPPDATA%\devin\cli\bin\devin.exe`), or `PATH`.
  - Spawns `devin acp` (with optional `--agent-type <CLI_DEVIN_AGENT_TYPE>`) in the resolved workspace directory (`resolveWorkspaceCwd(body)`), setting `DEVIN_PERMISSION_MODE="bypass"`.
- **JSON-RPC 2.0 State Machine (NDJSON over stdin/stdout)**:
  1. **Initialize**: Sends `{"jsonrpc":"2.0","method":"initialize","params":{"protocolVersion":"0.3","clientInfo":{"name":"9router","version":"1.0"},"capabilities":{}},"id":1}`.
  2. **Create Session**: On initialize response, sends `{"jsonrpc":"2.0","method":"session/new","params":{"cwd":"<workspaceCwd>","mcpServers":[],"model":"<model>"},"id":2}`.
  3. **Send Prompt**: On `session/new` response (extracting `result.sessionId`), sends `{"jsonrpc":"2.0","method":"session/prompt","params":{"sessionId":"<sessionId>","prompt":[{"type":"text","text":"<promptText>"}]},"id":3}`.
  4. **Permission Auto-Approval**: Responds to `session/request_permission` requests by selecting the first `"allow"` option (`{ outcome: { outcome: "selected", optionId: allow.optionId } }`).
  5. **Streaming Updates (`session/update` / `$/update`)**:
     - `agent_message_chunk` / `message_delta` / `text_delta` / `content_delta`: Emits OpenAI `chat.completion.chunk` SSE delta.
     - `_cognition.ai/agent_stopped` / `$/agent_stopped`: Closes stream and terminates subprocess.

---

## 3. Client Tools -> Temporary Stdio MCP Server Bridge
Because `devin acp` only invokes built-in tools and MCP tools (not OpenAI function-calling schemas), `DevinCliExecutor` bridges `body.tools` by:
1. Writing a Node.js MCP server script to `/tmp/9router-devin-client-tools.mjs`.
2. Prefixing each client tool name with `mcp_` (`toMcpToolName`) and passing tool schemas via `DEVIN_MCP_TOOLS` and historical tool results via `DEVIN_MCP_RESULTS`.
3. Creating a temporary `XDG_CONFIG_HOME` directory containing `devin/config.json` with `{ "mcpServers": { "clientTools": { "command": "<node>", "args": ["/tmp/9router-devin-client-tools.mjs"], "env": { ... } } } }`.
4. Watching `session/update` events for `tool_call` / `tool_call_update` with title `"Calling mcp_<name> from clientTools"`, stripping the `mcp_` prefix, emitting an OpenAI `tool_calls` SSE chunk with `finish_reason: "tool_calls"`, and ending the turn so the client can execute the tool.

---

## 4. Supported Models
| Model ID | Display Name | Context Length |
|---|---|---|
| `swe-1.6-fast`, `swe-1.6`, `swe-1.5-fast`, `swe-1.5` | SWE-1.6 / SWE-1.5 Series | — |
| `claude-opus-4.7-max`, `claude-opus-4.7-high`, `claude-opus-4.7-medium`, `claude-opus-4.7-low` | Claude Opus 4.7 Series | 200,000 |
| `claude-sonnet-4.6-thinking-1m` | Claude Sonnet 4.6 Thinking 1M | 1,000,000 |
| `claude-sonnet-4.6-thinking`, `claude-sonnet-4.6`, `claude-opus-4.6-thinking`, `claude-opus-4.6`, `claude-sonnet-4.5`, `claude-haiku-4.5` | Claude 4.5 / 4.6 Series | 200,000 |
| `gpt-5.5-xhigh`, `gpt-5.5-high`, `gpt-5.5-medium`, `gpt-5.5-low` | GPT-5.5 Series | 200,000 |
| `gpt-5.4-high`, `gpt-5.4-medium`, `gpt-5.4-low` | GPT-5.4 Series | 200,000 |
| `gpt-5.3-codex-high`, `gpt-5.3-codex-medium`, `gpt-5.3-codex-low` | GPT-5.3 Codex Series | 200,000 |
| `gpt-5.2-high`, `gpt-5.2-medium`, `gpt-5.2-low` | GPT-5.2 Series | 200,000 |
| `gemini-3.1-pro-high`, `gemini-3.1-pro-low`, `gemini-3.0-flash-high`, `gemini-2.5-pro` | Gemini Series | 1,000,000 |
| `deepseek-v4` | DeepSeek V4 | 1,048,576 |
| `kimi-k2.6` | Kimi K2.6 | 262,144 |
| `glm-5.1` | GLM-5.1 | 204,800 |

---

## 5. Verbatim Source Code Snippets

### `open-sse/executors/devin-cli.js` (ACP Handshake & MCP Bridge Excerpt)
```javascript
const CLIENT_TOOLS_MCP_SCRIPT = `
import readline from "node:readline";
const TOOLS = JSON.parse(process.env.DEVIN_MCP_TOOLS || "[]");
const RESULTS = JSON.parse(process.env.DEVIN_MCP_RESULTS || "{}");
const rl = readline.createInterface({ input: process.stdin });
function send(o){ process.stdout.write(JSON.stringify(o) + "\\n"); }
rl.on("line", (line) => {
  let m; try { m = JSON.parse(line); } catch { return; }
  if (m.method === "initialize") {
    send({ jsonrpc: "2.0", id: m.id, result: { protocolVersion: "2024-11-05", capabilities: { tools: {} }, serverInfo: { name: "clientTools", version: "1.0" } } });
  } else if (m.method === "tools/list") {
    send({ jsonrpc: "2.0", id: m.id, result: { tools: TOOLS } });
  } else if (m.method === "tools/call") {
    const name = m.params?.name || "";
    const seeded = RESULTS[name];
    const text = seeded !== undefined
      ? String(seeded)
      : "(awaiting client tool_result)";
    process.stderr.write("[client-tools] tool_call name=" + name + " seeded=" + (seeded !== undefined) + "\\n");
    send({ jsonrpc: "2.0", id: m.id, result: { content: [{ type: "text", text }] } });
  }
});
`.trimStart();
```
