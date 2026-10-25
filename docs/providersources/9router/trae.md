# Trae (`trae`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `trae`
- **Display Name**: `Trae`
- **Category**: `oauth`
- **Service Kinds**: `["llm"]`
- **Executor**: `TraeExecutor` (`open-sse/executors/trae.js`)
- **OAuth Service**: `src/lib/oauth/providers/trae.js`
- **Website**: `https://www.trae.ai`

---

## 2. Endpoints & Authentication

| Service | Endpoint |
|---|---|
| **Create Session** | `POST https://core-normal.trae.ai/api/remote/v1/chat_sessions` |
| **Send Message** | `POST https://core-normal.trae.ai/api/remote/v1/chat_sessions/{session_id}/messages` |
| **Stream Events** | `GET https://core-normal.trae.ai/api/remote/v1/chat_sessions/{session_id}/events?reply_to_message_id={message_id}` |
| **Login Guidance** | `POST https://api.marscode.com/cloudide/api/v3/trae/GetLoginGuidance` |
| **Exchange Token** | `POST https://api.marscode.com/cloudide/api/v3/trae/oauth/ExchangeToken` |
| **User Info** | `POST https://api.marscode.com/cloudide/api/v3/trae/GetUserInfo` |

### Headers (`open-sse/executors/trae.js`)

```javascript
const TRAE_HEADERS = {
  "X-Trae-Client-Type": "web",
  Origin: "https://solo.trae.ai",
  Referer: "https://solo.trae.ai/",
  "User-Agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
};
```
- **Authorization Header**: `Authorization: Cloud-IDE-JWT <jwtToken>`

---

## 3. OAuth Flow (`src/lib/oauth/providers/trae.js`)

- **Client ID**: `ono9krqynydwx5`
- **Auth URL**: `https://www.trae.ai/authorization`
- Calls `GetLoginGuidance` with `{ IdeVersion: "99.99.99", AppVersion: "1.0.0", Platform: "Mac", Region: "SG", Scene: "trae" }` to resolve regionally routed `AuthDomain` and `TargetHost`, then exchanges `code` via `ExchangeToken` (`{ RefreshToken: "", ClientID: "ono9krqynydwx5", Code: code }`).

---

## 4. Two-Step Remote Session & Event Stream (`open-sse/executors/trae.js`)

```javascript
async execute({ model, body, stream, credentials, signal, log }) {
  const token = credentials.accessToken;
  if (!token) throw new Error("Trae: missing JWT token");

  const prompt = extractPrompt(body.messages);
  if (!prompt) throw new Error("Trae: empty prompt");

  const createRes = await fetch(`${TRAE_BASE}/chat_sessions`, {
    method: "POST",
    headers: {
      ...TRAE_HEADERS,
      Authorization: `Cloud-IDE-JWT ${token}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      agent_name: "SOLO Coder",
      prompt,
      model_name: model,
      type: "local",
    }),
    signal,
  });

  const { session_id, message_id } = await createRes.json();

  const eventsUrl = `${TRAE_BASE}/chat_sessions/${session_id}/events?reply_to_message_id=${message_id}`;
  const eventsRes = await fetch(eventsUrl, {
    headers: {
      ...TRAE_HEADERS,
      Authorization: `Cloud-IDE-JWT ${token}`,
      Accept: "text/event-stream",
    },
    signal,
  });

  if (stream !== false) {
    return {
      response: new Response(this._transformToOpenAISSE(eventsRes.body, model, session_id, signal), {
        status: 200,
        headers: {
          "Content-Type": "text/event-stream",
          "Cache-Control": "no-cache",
          Connection: "keep-alive",
        },
      }),
      url: eventsUrl,
      headers: TRAE_HEADERS,
      transformedBody: body,
    };
  }
}
```

### Supported Models (`open-sse/config/providerModels.js`)
- `Gemini-3.1-Pro-Preview`
- `Gemini-3-Pro-Preview`
- `Gemini-3-Flash-Preview`
- `Gemini-2.5-Flash`
- `Claude-Sonnet-4.6`
- `Claude-Opus-4.6`
- `GPT-5.2`
- `GPT-5.1`
- `DeepSeek-V3.1-Terminus`
- `Kimi-K2.5`
- `GLM-5`
- `MiniMax-M2.5`
