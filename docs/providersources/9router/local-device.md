# Local Device TTS (`local-device`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `local-device`
- **Alias**: `local-device`
- **Display Name**: Local Device
- **Category**: `freeTier`
- **Auth Type**: `none` (`noAuth: true`)
- **Service Kinds**: `["tts"]`
- **Media Priority**: `5`
- **Source Files**:
  - `open-sse/providers/registry/local-device.js`
  - `open-sse/handlers/ttsProviders/localDevice.js`

---

## 2. Subprocess Execution & Audio Conversion (`localDevice.js`)
- **Voice Discovery (`fetchLocalDeviceVoices`)**:
  - **macOS / Unix**: Spawns `say -v ?` and parses voice name and locale (`en_US`, etc.).
  - **Windows**: Spawns `powershell.exe -NoProfile -NonInteractive -WindowStyle Hidden -Command "Add-Type -AssemblyName System.Speech; ..."` to query `System.Speech.Synthesis.SpeechSynthesizer.GetInstalledVoices()`.
- **Speech Synthesis (`synthesizeMacOrWin`)**:
  - Creates a temporary directory in `os.tmpdir()` (`tts-*`).
  - Spawns `say [-v <voiceId>] -o <dir>/out.aiff <text>`.
  - Converts AIFF to MP3 via `ffmpeg -y -i <dir>/out.aiff -codec:a libmp3lame -qscale:a 4 <dir>/out.mp3`.
  - Reads `<dir>/out.mp3`, encodes as Base64 (`{ base64, format: "mp3" }`), and cleans up the temporary directory.

---

## 3. Verbatim Source Code Snippets

### `open-sse/providers/registry/local-device.js`
```javascript
export default {
  id: "local-device",
  alias: "local-device",
  display: {
    name: "Local Device",
    icon: "speaker",
    color: "#64748B",
    textIcon: "LD"
  },
  category: "freeTier",
  authType: "none",
  serviceKinds: [
    "tts"
  ],
  mediaPriority: 5,
  noAuth: true,
  ttsConfig: {
    baseUrl: "local-device",
    authType: "none",
    authHeader: "none",
    format: "local-device",
    models: []
  }
};
```

### `open-sse/handlers/ttsProviders/localDevice.js` (Excerpt)
```javascript
async function synthesizeMacOrWin(text, voiceId) {
  const dir = await mkdtemp(join(tmpdir(), "tts-"));
  const aiffPath = join(dir, "out.aiff");
  const mp3Path = join(dir, "out.mp3");
  try {
    const args = voiceId ? ["-v", voiceId, "-o", aiffPath, text] : ["-o", aiffPath, text];
    await execFileAsync("say", args);
    await execFileAsync("ffmpeg", ["-y", "-i", aiffPath, "-codec:a", "libmp3lame", "-qscale:a", "4", mp3Path]);
    const buf = await readFile(mp3Path);
    return buf.toString("base64");
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
}
```
