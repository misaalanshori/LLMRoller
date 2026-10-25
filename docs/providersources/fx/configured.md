# Configured Provider (`configured` / `openai-chat-completions`) Specification (`fx`)

## 1. Overview
- **Provider ID**: `.configured` (slug: `"configured"`)
- **Name**: `Configured provider` (`openai-chat-completions`)
- **Source Files**:
  - `src/builtins/providers.zig`
  - `src/core/config/configured_provider.zig`
  - `src/gateway/chat_completions.zig`
  - `src/gateway/chat_completions_protocol.zig`
  - `sdk/custom-provider.js`

## 2. Configuration & Endpoint Resolution
- **Supported Protocol**: `openai-chat-completions` (`Protocol.@"openai-chat-completions"`).
- **Endpoint Construction (`Definition.chat_url` & `chatUrlOf`)**:
  - Concatenates `<base_url>/chat/completions`.
  - Enforces HTTPS for remote hosts (`https://`), permitting plain HTTP (`http://`) only on loopback hosts (`localhost`, `127.0.0.1`, `[::1]`).
  - Rejects URLs containing embedded credentials, query strings, or fragments.
  - Reserved provider names (`gateway`, `codex`, `grok`) are disallowed.

## 3. Authentication & Headers (`src/gateway/chat_completions.zig`)
- **Auth Modes (`configured_provider.Auth`)**:
  1. `.none`: Omits `Authorization` header (rejects requests if a token is unexpectedly supplied).
  2. `.bearer`: Resolves bearer token from the configured environment variable slot (or SDK host `fetch` edge in `sdk/custom-provider.js`) and sends `Authorization: Bearer <token>`.
- **Headers**:
  - `Authorization: Bearer <token>` (when `auth.type == "bearer"`)
  - `Content-Type: application/json`
  - `Accept: text/event-stream`
  - `User-Agent: fx/<version>`

## 4. Wire Format & Replay Binding (`src/gateway/chat_completions_protocol.zig`)
- **Request Payload**:
  - `POST <base_url>/chat/completions` with `"stream": true`, `"stream_options": {"include_usage": true}`, `"model"`, `"messages"`, `"tools"`, `"max_completion_tokens"` (when `max_output_tokens` is set), and optional `"tool_choice"` (controlled by `tool_choice_mode: .omit | .send`).
- **SHA-256 Authority Binding (`Definition.binding_identity`)**:
  - Computes a SHA-256 digest over `"fx-configured-provider-v1"`, `id`, `protocol`, `base_url`, and `auth` slot so `reasoning_details` provider replay states are bound to the exact endpoint authority and stripped if the endpoint changes.

## 5. Source Implementation (`src/core/config/configured_provider.zig` & `src/gateway/chat_completions.zig`)
```zig
pub const Protocol = enum { @"openai-chat-completions" };
pub const ToolChoiceMode = enum { omit, send };

pub const Auth = union(enum) {
    none,
    bearer: []const u8,
};

pub const Definition = struct {
    id: []const u8,
    protocol: Protocol,
    base_url: []const u8,
    auth: Auth,
    tool_choice_mode: ToolChoiceMode = .omit,
    reviewer_model: ?[]const u8 = null,
    model_metadata: []const ModelMetadata = &.{},

    pub fn chat_url(self: Definition, alloc: Allocator) Allocator.Error![]u8 {
        return std.mem.concat(alloc, u8, &.{ self.base_url, "/chat/completions" });
    }
};
```
