# Muse Web (`muse-web`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `muse-web`
- **Priority**: `220`
- **Primary Alias**: `muse-web`
- **Additional Aliases**: `["mu"]`
- **UI Alias**: `muse-web`
- **Display Name**: Muse Web (muse.ai Hatch)
- **Category**: `webCookie`
- **Auth Type**: `cookie` (`hatch_sess`, `hatch_gw`, `hatch_vml`, `hatch_native_auth_device`)
- **Service Kinds**: `["image", "video"]` (`videoConfig: { executorDriven: true }`; `muse-chat` LLM is also implemented in `MuseWebExecutor`)
- **Website**: `https://muse.ai`
- **Executor**: `MuseWebExecutor` (`open-sse/executors/muse-web.js` and `open-sse/executors/muse-web/*.js`)
- **Source Files**:
  - `open-sse/providers/registry/muse-web.js`
  - `open-sse/executors/muse-web.js`
  - `open-sse/executors/muse-web/session.js`
  - `open-sse/executors/muse-web/dom.js`
  - `open-sse/executors/muse-web/prompt.js`

---

## 2. Session Renewal, Cloud VM Wake & Playwright DOM Automation
1. **Session Renewal (`renewSession` in `session.js`)**:
   - `GET https://muse.ai/api/session`
   - Headers: `User-Agent: Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36`, `Origin: https://muse.ai`, `Referer: https://muse.ai/thread/new`, `Accept: application/json`, `Cookie: <raw_cookies>`.
   - Merges rotated `Set-Cookie` headers into the cookie jar and reads `{ status, vm_id, vm_state }`.
2. **Cloud Workspace VM Wake**:
   - If `vm_id` is present and `vm_state !== "DISABLED"`, calls `POST https://muse.ai/api/hatch/vm/wake` with `{ vm_id, retry_count: 0, connect_attempt_id: <uuid> }`.
3. **Playwright Thread Automation**:
   - Seeds the rotated cookies into `.muse.ai` on a pooled browser context (`acquireBrowserContext`) and opens `https://muse.ai/thread/new`.
   - Attaches up to 8 input images (`attachImages`), fills the prompt input (`dom.fillInputScript`), and clicks send (`dom.CLICK_SEND`).
   - Polls DOM (`dom.CHAT_STATE`), automatically denying tool approval prompts (`denyApprovals`), and waits for text stability (`pollChat`) or inline media attachments (`waitForMedia`, with fallback follow-up prompt `"Please attach the final <kind> file here in the chat"` if the agent leaves the generated image/video only in its workspace).

---

## 3. Supported Models
| Model ID | Display Name | Kind | Parameters |
|---|---|---|---|
| `muse-chat` | Muse Chat | `llm` | — |
| `muse-image` | Muse Image | `image` | `["n", "size"]` |
| `muse-video` | Muse Video | `video` | `["size", "seconds", "image"]` |

---

## 4. Verbatim Source Code Snippets

### `open-sse/providers/registry/muse-web.js`
```javascript
export default {
  id: "muse-web",
  priority: 220,
  alias: "muse-web",
  aliases: [
    "mu",
  ],
  uiAlias: "muse-web",
  display: {
    name: "Muse Web",
    icon: "auto_awesome",
    color: "#7C3AED",
    textIcon: "MU",
    website: "https://muse.ai",
  },
  category: "webCookie",
  authType: "cookie",
  serviceKinds: ["image", "video"],
  videoConfig: { executorDriven: true },
  authHint: "Paste your muse.ai cookies (hatch_sess=...; hatch_gw=...; hatch_vml=...; hatch_native_auth_device=...) from a logged-in muse.ai session",
  transport: {
    baseUrl: "https://muse.ai",
    format: "openai",
    authType: "cookie",
  },
  models: [
    { id: "muse-chat", name: "Muse Chat" },
    { id: "muse-image", name: "Muse Image", params: ["n", "size"], kind: "image" },
    { id: "muse-video", name: "Muse Video", params: ["size", "seconds", "image"], kind: "video" },
  ],
};
```
