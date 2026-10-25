# Uncensored Chat Web (`uc-web`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `uc-web`
- **Display Name**: `Uncensored Chat (Web)`
- **Alias**: `ucw`
- **Category**: `cookie`
- **Service Kinds**: `["image", "video"]`
- **Executor**: `UcWebExecutor` (`open-sse/executors/uc-web.js`)
- **Website**: `https://uncensored.com`

---

## 2. Endpoints & Clerk Session Token Flow

| Step | Endpoint |
|---|---|
| **Clerk Client Inspection** | `GET https://clerk.uncensored.com/v1/client?_clerk_js_version=5.127.1` |
| **Clerk 60s Session Mint** | `POST https://clerk.uncensored.com/v1/client/sessions/{sid}/tokens?_clerk_js_version=5.127.1` |
| **Image Generation** | `POST https://internal.chatuncensored.ai/v2/image-gen` |
| **Signed Upload URL** | `POST https://internal-6.pubyar.com/generate-signed-url` |
| **Image-to-Video Generation** | `POST https://internal.chatuncensored.ai/image_to_video` |

---

## 3. Clerk Session Refresh & Image/Video Polling (`open-sse/executors/uc-web.js`)

```javascript
async function mintSessionToken(clientCookie, sessionId) {
  let sid = sessionId;
  if (!sid) {
    sid = await fetchSessionId(clientCookie);
  }
  const url = `${CLERK_BASE}/v1/client/sessions/${sid}/tokens?_clerk_js_version=${CLERK_VERSION}`;
  const res = await fetch(url, {
    method: "POST",
    headers: {
      "Content-Type": "application/x-www-form-urlencoded",
      Cookie: `__client=${clientCookie}`,
      Origin: ORIGIN,
      Referer: `${ORIGIN}/`,
      "User-Agent": BROWSER_UA,
    },
    body: "organization_id=",
  });
  const data = await res.json();
  const jwt = data?.jwt;
  // Extract rotated __client from Set-Cookie header if Clerk issued a new one
  return { jwt, sessionId: sid, rotatedClient };
}
```

### Supported Models
- **Image (`imageRegistry.js`)**: `uncensored-image` (`Uncensored Image`, aspect ratios `16:9`, `9:16`, `1:1`, `4:3`, `3:4`)
- **Video (`videoRegistry.js`)**: `uncensored-video` (`Uncensored Video (I2V)`, durations 3s/5s/8s, requires input image)
