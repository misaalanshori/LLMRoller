# Anthropic-Compatible Dynamic Provider (`anthropic-compatible`) Specification (`9router`)

## 1. Overview & Identification

- **Provider Prefix**: `anthropic-compatible-*` (`ANTHROPIC_COMPATIBLE_PREFIX = "anthropic-compatible-"`)
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Service Kinds**: `["llm"]`

---

## 2. Dynamic Endpoint & Dual Authentication (`open-sse/executors/default.js`)

- **Endpoint**: `POST ${credentials.providerSpecificData.baseUrl}/messages` (defaults to `https://api.anthropic.com/v1/messages`)
- **Dual Header Authentication**:
  - Sets `x-api-key: <apiKey>` and `anthropic-version: 2023-06-01`.
  - For non-official Anthropic base URLs (`!baseUrl.includes("api.anthropic.com")`), **also** sets `Authorization: Bearer <apiKey>` so third-party Anthropic-compatible gateways reading either header succeed.
  - Strips first-party Claude Code identity headers (`anthropic-dangerous-direct-browser-access`, `x-app`, and the `claude-code-20250219` beta flag) when targeting third-party gateways, while preserving `Anthropic-Beta` flags like `context-management-2025-06-27` when `model` starts with `claude-`.

```javascript
if (this.provider?.startsWith?.("anthropic-compatible-")) {
  const baseUrl = credentials?.providerSpecificData?.baseUrl || ANTHROPIC_COMPAT_BASE;
  const normalized = baseUrl.replace(/\/$/, "");
  return `${normalized}/messages`;
}
```
