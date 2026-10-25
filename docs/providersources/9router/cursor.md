# Cursor IDE (`cursor`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `cursor`
- **Priority**: `50`
- **Alias / UI Alias**: `cu`
- **Display Name**: Cursor IDE
- **Category**: `oauth` (`flowType: "import_token"` — extracted from Cursor's local SQLite DB keys `cursorAuth/accessToken` and `storage.serviceMachineId`)
- **Website**: `https://cursor.com`
- **Executor**: `CursorExecutor` (`open-sse/executors/cursor.js`)
- **Source Files**:
  - `open-sse/providers/registry/cursor.js`
  - `open-sse/executors/cursor.js`
  - `open-sse/utils/cursorChecksum.js`
  - `open-sse/utils/cursorProtobuf.js`
  - `src/lib/oauth/providers/cursor.js`

---

## 2. Dual-RPC Endpoints (ConnectRPC + Binary Protobuf over HTTP/2)
`CursorExecutor` implements two distinct ConnectRPC/Protobuf pipelines:
1. **Primary AgentService Duplex HTTP/2 Stream (for text & tool turns, `isAgentCapableRequest(body) === true`)**:
   - **Endpoint**: `POST https://agent.api5.cursor.sh/agent.v1.AgentService/Run`
   - **Transport**: Bidirectional Node `http2` duplex stream (`openAgentHttp2Stream`).
   - **Protocol**: Sends `agent.v1.AgentClientMessage.run_request` frames (5-byte ConnectRPC envelope: 1-byte flags + 4-byte big-endian length + Protobuf payload). During the stream, responds inline on the same duplex HTTP/2 stream to server-initiated RPCs:
     - `KvServerMessage` (field 4): Acknowledges KV blob reads/writes via `encodeKvClientMessage`.
     - `ExecServerMessage` (field 2, subfield 10 `request_context`): Sends `createRequestContextResponse` ack.
     - `ExecServerMessage` (field 2, subfield 11 `mcp_tool_call`): Extracts MCP tool call via `decodeMcpArgs` and emits an OpenAI `tool_call` SSE chunk.
     - Built-in IDE tool probes (fields 2, 3, 4, 5, 7, 8, 9, 16, 20, 23): Rejects via `rejectExecRequest` with `"Tool not available in this environment. Use the MCP tools provided instead."` so the model falls back to client MCP tools.
2. **Legacy ChatService Endpoint (for turns containing image parts)**:
   - **Endpoint**: `POST https://api2.cursor.sh/aiserver.v1.ChatService/StreamUnifiedChatWithTools`
   - **Transport**: HTTP/2 (`makeHttp2Request`) or `proxyAwareFetch` when proxy/relay is enabled.
3. **Model Discovery & Usage Endpoints**:
   - `modelsEndpoint`: `https://agent.api5.cursor.sh/agent.v1.AgentService/GetUsableModels`
   - `usage.url`: `https://api2.cursor.sh/aiserver.v1.DashboardService/GetCurrentPeriodUsage`
   - `usage.planInfoUrl`: `https://api2.cursor.sh/aiserver.v1.DashboardService/GetPlanInfo`
   - `usage.authUsageUrl`: `https://api2.cursor.sh/auth/usage`

---

## 3. Authentication, Headers & Jyh Cipher Checksum (`cursorChecksum.js`)
- **Token Normalization**: If `accessToken` contains `::`, strips the prefix (`accessToken.split("::")[1]`).
- **`x-cursor-checksum` (Jyh Cipher)**:
  - Computes `timestamp = Math.floor(Date.now() / 1000000)` as a 6-byte big-endian `Uint8Array`.
  - Iterates `i = 0..5` with initial rolling key `t = 165`:
    `byteArray[i] = ((byteArray[i] ^ t) + (i % 256)) & 0xFF; t = byteArray[i];`
  - Encodes `byteArray` using URL-safe Base64 without padding and concatenates `${encoded}${machineId}`.
- **Full Request Headers (`buildCursorHeaders`)**:
  - `authorization`: `Bearer <cleanToken>`
  - `connect-accept-encoding`: `gzip`
  - `connect-protocol-version`: `1`
  - `content-type`: `application/connect+proto`
  - `user-agent`: `connect-es/1.6.1`
  - `x-amzn-trace-id`: `Root=<uuid>`
  - `x-client-key`: `SHA256(cleanToken)`
  - `x-cursor-checksum`: `<jyhBase64><machineId>`
  - `x-cursor-client-version`: `3.12.17`
  - `x-cursor-client-commit`: `0fb762053c34788bb7760d5673f8a6d4c8589d50`
  - `x-cursor-client-type`: `ide`
  - `x-cursor-client-os`: `windows` | `macos` | `linux`
  - `x-cursor-client-arch`: `aarch64` | `x64`
  - `x-cursor-client-device-type`: `desktop`
  - `x-cursor-config-version`: `<uuid>`
  - `x-cursor-timezone`: `<IANA timezone>`
  - `x-ghost-mode`: `"true"` or `"false"`
  - `x-request-id`: `<uuid>`
  - `x-session-id`: `uuidv5(cleanToken, uuidv5.DNS)`

---

## 4. Supported Models
| Model ID | Display Name |
|---|---|
| `default` | Auto (Server Picks — resolved via `resolveCursorModels`) |
| `claude-4.5-opus-high-thinking` | Claude 4.5 Opus High Thinking |
| `claude-4.5-opus-high` | Claude 4.5 Opus High |
| `claude-4.5-sonnet-thinking` | Claude 4.5 Sonnet Thinking |
| `claude-4.5-sonnet` | Claude 4.5 Sonnet |
| `claude-4.5-haiku` | Claude 4.5 Haiku |
| `claude-4.5-opus` | Claude 4.5 Opus |
| `gpt-5.2-codex` | GPT 5.2 Codex |
| `claude-4.6-opus-max` | Claude 4.6 Opus Max |
| `claude-4.6-sonnet-medium-thinking` | Claude 4.6 Sonnet Medium Thinking |
| `kimi-k2.5` | Kimi K2.5 |
| `gemini-3-flash-preview` | Gemini 3 Flash Preview |
| `gpt-5.2` | GPT 5.2 |
| `gpt-5.3-codex` | GPT 5.3 Codex |

---

## 5. Verbatim Source Code Snippets

### `open-sse/utils/cursorChecksum.js`
```javascript
export function generateCursorChecksum(machineId) {
  const timestamp = Math.floor(Date.now() / 1000000);
  const byteArray = new Uint8Array([
    (timestamp >> 40) & 0xFF,
    (timestamp >> 32) & 0xFF,
    (timestamp >> 24) & 0xFF,
    (timestamp >> 16) & 0xFF,
    (timestamp >> 8) & 0xFF,
    timestamp & 0xFF
  ]);

  let t = 165;
  for (let i = 0; i < byteArray.length; i++) {
    byteArray[i] = ((byteArray[i] ^ t) + (i % 256)) & 0xFF;
    t = byteArray[i];
  }

  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
  let encoded = "";

  for (let i = 0; i < byteArray.length; i += 3) {
    const a = byteArray[i];
    const b = i + 1 < byteArray.length ? byteArray[i + 1] : 0;
    const c = i + 2 < byteArray.length ? byteArray[i + 2] : 0;

    encoded += alphabet[a >> 2];
    encoded += alphabet[((a & 3) << 4) | (b >> 4)];

    if (i + 1 < byteArray.length) {
      encoded += alphabet[((b & 15) << 2) | (c >> 6)];
    }
    if (i + 2 < byteArray.length) {
      encoded += alphabet[c & 63];
    }
  }

  return `${encoded}${machineId}`;
}
```
