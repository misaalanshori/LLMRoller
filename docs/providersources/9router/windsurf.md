# Windsurf (`windsurf`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `windsurf`
- **Display Name**: `Windsurf`
- **Alias**: `ws`
- **Category**: `oauth` (`authModes: ["oauth", "apikey"]`)
- **Service Kinds**: `["llm"]`
- **Executor**: `WindsurfExecutor` (`open-sse/executors/windsurf.js`)
- **Website**: `https://windsurf.com`

---

## 2. Endpoints, Authentication Chain & gRPC-Web Wire Protocol

| Endpoint | URL |
|---|---|
| **gRPC-Web Chat Endpoint** | `POST https://server.codeium.com/exa.language_server_pb.LanguageServerService/GetChatMessage` |
| **Devin `auth1_` Self-Serve Server** | `https://server.self-serve.windsurf.com` |
| **Firebase Sign-In** | `POST https://identitytoolkit.googleapis.com/v1/accounts:signInWithPassword?key=AIzaSyDsOl-1XpT5err0Tcn0TFFod1H8gVGIycY` |
| **Seat Management RegisterUser** | `POST https://register.windsurf.com/exa.seat_management_pb.SeatManagementService/RegisterUser` |
| **Quota Status (Connect RPC Proto)** | `POST https://windsurf.com/_backend/exa.seat_management_pb.SeatManagementService/GetPlanStatus` |

### Headers (`open-sse/providers/registry/windsurf.js`)
```javascript
headers: {
  "Content-Type": "application/grpc-web+proto",
  "Accept": "application/grpc-web+proto",
  "X-Grpc-Web": "1",
  "Authorization": "Bearer <apiKey>",
}
```

### 4-Path Authentication Chain
1. **OAuth Web** (`clientId: "3GUryQ7ldAeKEuD2obYnppsnmj58eP5u"`) -> Firebase JWT -> `RegisterUser` (`{ firebase_id_token }`) -> yields `{ apiKey, apiServerUrl, name }`.
2. **Direct API Key** (`sk-ws-...`) -> placed in `Metadata.api_key` protobuf field and `Authorization: Bearer` header.
3. **Firebase JWT** (`eyJ...`) -> exchanged via `RegisterUser`.
4. **Devin `auth1_...` Token** -> self-serve chain yielding `ide_token` against `https://server.self-serve.windsurf.com`.

---

## 3. Protobuf Framing & Model Alias Mapping (`open-sse/executors/windsurf.js`)

- Encodes `GetChatMessageRequest` with `Metadata` (`ide_name = "windsurf"`, `ide_version = "3.14.0"`, `extension_version = "3.14.0"`, `api_key`, `locale = "en-US"`, `session_id`) wrapped in a 5-byte gRPC-Web frame header (`flag = 0x00` + 4-byte big-endian length).
- Parses streamed gRPC-Web frames (`0x00` data frames with `CompletionChunk` protobuf messages and `0x80` trailer frames with `grpc-status` / `grpc-message`) and converts them into OpenAI SSE `chat.completion.chunk` events.
- Maps 65+ dot-notation catalog IDs (`swe-1.6-fast`, `claude-opus-4.7-max`, `claude-sonnet-4.6-thinking-1m`, `gpt-5.5-xhigh-fast`, `gpt-5.4-xhigh-fast`, `gpt-5.3-codex-xhigh-fast`, `gemini-3.1-pro-high`, `deepseek-v4`, `kimi-k2.6`, `glm-5.1`) to Windsurf wire `modelUid` values via `MODEL_ALIAS_MAP`.
