# Qoder (`qoder`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `qoder`
- **Priority**: `30`
- **Alias / UI Alias**: `qd`
- **Display Name**: Qoder
- **Category**: `oauth`
- **Auth Modes**: `["oauth", "apikey"]` (Device OAuth `dt-...` or Personal Access Token `pt-...` from `https://qoder.com/account/integrations`)
- **Website**: `https://qoder.com`
- **Executor**: `QoderExecutor` (`open-sse/executors/qoder.js`)
- **Source Files**:
  - `open-sse/providers/registry/qoder.js`
  - `open-sse/executors/qoder.js`
  - `open-sse/shared/qoder/cosy.js`
  - `open-sse/shared/qoder/encoding.js`
  - `src/lib/oauth/providers/qoder.js`

---

## 2. Endpoints & OAuth / PAT Exchange
- **Inference Endpoint**:
  - `POST https://api3.qoder.sh/algo/api/v2/service/pro/sse/agent_chat_generation?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1`
- **OAuth & API Endpoints**:
  - `openApiBaseUrl`: `https://openapi.qoder.sh`
  - `centerBaseUrl`: `https://center.qoder.sh`
  - `chatBaseUrl`: `https://api3.qoder.sh`
  - `deviceTokenUrl`: `https://openapi.qoder.sh/api/v1/deviceToken/poll`
  - `refreshUrl`: `https://center.qoder.sh/algo/api/v3/user/refresh_token`
  - `userInfoUrl`: `https://openapi.qoder.sh/api/v1/userinfo`
  - `quotaUsageUrl`: `https://openapi.qoder.sh/api/v2/quota/usage`
  - `loginUrl`: `https://qoder.com/device/selectAccounts`
- **PAT (`pt-...`) to Job Token Exchange (`resolveQoderCredentials`)**:
  - Exchanges a Personal Access Token (`pt-...`) for a short-lived job token (`jt-...`) and resolves `userId` so COSY signing can execute.

---

## 3. WAF-Bypass Body Obfuscation (`qoderEncodeBody`) & COSY Signing (`buildCosyHeaders`)

### 1. WAF-Bypass Body Encoding (`open-sse/shared/qoder/encoding.js`)
1. Base64-encodes the UTF-8 JSON request body using standard Base64 (`QODER_STD_ALPHABET`).
2. Splits the Base64 string of length `n` at `a = Math.floor(n / 3)` and rearranges the thirds into `[tail][mid][head]`:
   `std.slice(n - a) + std.slice(a, n - a) + std.slice(0, a)`.
3. Substitutes every Base64 character using `QODER_CUSTOM_ALPHABET`:
   `"_doRTgHZBKcGVjlvpC,@aFSx#DPuNJme&i*MzLOEn)sUrthbf%Y^w.(kIQyXqWA!"` and maps `'='` -> `'$'`.

### 2. COSY Hybrid RSA + AES-128-CBC + MD5 Headers (`open-sse/shared/qoder/cosy.js`)
1. Generates a 16-byte AES key (`uuidv4().slice(0, 16)`), encrypts `{ uid: userId, security_oauth_token: authToken, name, aid: "", email }` with AES-128-CBC (`iv = keyBytes`, PKCS#7 padding) -> `info` (Base64).
2. Encrypts the 16-byte AES key with `QODER_RSA_PUBLIC_KEY` (`RSA_PKCS1_PADDING`) -> `cosyKey` (Base64).
3. Base64-encodes `{ version: "v1", requestId, info, cosyVersion: QODER_IDE_VERSION, ideVersion: "" }` -> `payloadB64`.
4. Computes `sig = MD5("${payloadB64}\n${cosyKey}\n${timestamp}\n${encodedBodyStr}\n${sigPath}")` where `sigPath` is the URL pathname with leading `/algo` stripped (`/api/v2/service/pro/sse/agent_chat_generation`).
5. Emits headers:
   - `Authorization`: `Bearer COSY.${payloadB64}.${sig}`
   - `Cosy-Key`: `<cosyKey>`
   - `Cosy-User`: `<userId>`
   - `Cosy-Date`: `<unix_seconds>`
   - `Cosy-Version`: `QODER_IDE_VERSION`
   - `Cosy-Machineid` & `Cosy-Machinetoken`: `<machineId>`
   - `Cosy-Machinetype`, `Cosy-Machineos`, `Cosy-Clienttype`, `Cosy-Clientip: "127.0.0.1"`
   - `Cosy-Bodyhash`: `MD5(encodedBodyBuf)`
   - `Cosy-Bodylength`: `String(encodedBodyBuf.length)`
   - `Cosy-Sigpath`: `<sigPath>`
   - `Cosy-Data-Policy`, `Cosy-Organization-Id: ""`, `Cosy-Organization-Tags: ""`, `Login-Version`
   - `X-Model-Key`: `<qoderKey>`, `X-Model-Source`: `<modelSource>`, `Accept-Encoding`: `"identity"`

---

## 4. Supported Models
| Model ID | Display Name |
|---|---|
| `ultimate`, `auto`, `performance`, `efficient`, `lite` | Tiered Routing Modes |
| `qmodel_38max`, `qmodel_latest`, `qmodel`, `qfmodel` | Qwen 3.7 / 3.8 Series |
| `kmodel_latest`, `kmodel` | Kimi K3 / K2.7 Code |
| `gmodel`, `gfmodel` | GLM 5.3 / 5.3 Flash |
| `dmodel`, `dfmodel` | DeepSeek V4 Pro / Flash |
| `mmodel` | MiniMax M3 |
| `smodel`, `cmodel` | Sonus / Cantus (Frontier Reasoning) |

---

## 5. Verbatim Source Code Snippets

### `open-sse/shared/qoder/encoding.js`
```javascript
const QODER_STD_ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const QODER_CUSTOM_ALPHABET = "_doRTgHZBKcGVjlvpC,@aFSx#DPuNJme&i*MzLOEn)sUrthbf%Y^w.(kIQyXqWA!";

const QODER_S2C = (() => {
  const table = new Int16Array(128).fill(-1);
  for (let i = 0; i < 64; i++) {
    table[QODER_STD_ALPHABET.charCodeAt(i)] = QODER_CUSTOM_ALPHABET.charCodeAt(i);
  }
  table["=".charCodeAt(0)] = "$".charCodeAt(0);
  return table;
})();

export function qoderEncodeBody(plaintext) {
  const buf = Buffer.isBuffer(plaintext)
    ? plaintext
    : typeof plaintext === "string"
      ? Buffer.from(plaintext, "utf8")
      : Buffer.from(plaintext);

  const std = buf.toString("base64");
  const n = std.length;
  const a = Math.floor(n / 3);
  const rearranged = std.slice(n - a) + std.slice(a, n - a) + std.slice(0, a);

  const out = Buffer.alloc(n);
  for (let i = 0; i < n; i++) {
    const c = rearranged.charCodeAt(i);
    if (c < 128 && QODER_S2C[c] >= 0) {
      out[i] = QODER_S2C[c];
    } else {
      out[i] = c;
    }
  }
  return out.toString("latin1");
}
```
