# tauri-plugin-mpv

Composite a real **mpv** video surface *beneath* a transparent Tauri v2 webview, so your HTML UI
draws over live, hardware-decoded video with alpha.

```ts
import { MpvVideo } from 'tauri-plugin-mpv-api'

// Looks and behaves like an <video> element.
const video = await MpvVideo.create({ target: document.getElementById('player') })
video.src = 'file:///movies/big-buck-bunny.mkv'
await video.play()
video.ontimeupdate = () => console.log(video.currentTime, '/', video.duration)
```

## Why another mpv plugin

Two mpv plugins for Tauri already exist ([`tauri-plugin-mpv`][n1] and [`tauri-plugin-libmpv`][n2],
both by nini22P). They work well on Windows. This one exists to fix three specific things.

**1. It works off Windows.** Both existing plugins embed mpv with `--wid` (native window
embedding). mpv's own maintainers [warn][mpv-embed] that `--wid` has "various platform-specific
behavior and problems (in particular on OSX)", and on Linux it needs an X11 window id that simply
does not exist under Wayland or with GTK client-side decorations. Both plugins document Linux
embedding as not working.

This plugin uses libmpv's **render API** instead: mpv draws into an OpenGL surface the plugin
owns and positions behind the webview. No window id is needed anywhere, so the Linux backend
works identically on X11 and Wayland.

**2. No build-time native dependency, and nothing to hand-download.** libmpv is resolved at
*runtime* with `dlopen`. There is no pkg-config, no `.lib`, no bindgen, and no clang needed to
build — and no per-platform shim binary for your users to fetch. A missing libmpv is a typed
error you can catch and render, not a link failure on someone else's machine.

**3. It is drop-in compatible with `<video>`.** `MpvVideo` implements the `HTMLMediaElement`
surface your player UI already talks to, so adopting a native decoder is a one-line change
instead of a rewrite of your playback layer.

[n1]: https://github.com/nini22P/tauri-plugin-mpv
[n2]: https://github.com/nini22P/tauri-plugin-libmpv
[mpv-embed]: https://github.com/mpv-player/mpv-examples/tree/master/libmpv

## Platform support

| Platform | Status | Notes |
|---|---|---|
| **Linux** (X11 + Wayland) | ✅ **Verified** | `GtkGLArea` inside a `GtkOverlay`, below the WebKit webview. A headless pixel test verifies compositing order, geometry, colour and vertical orientation end-to-end. |
| **Windows** | ⚠️ **UNVERIFIED — needs testing on real hardware** | Child `HWND` with a WGL context, z-ordered beneath WebView2. Compile-checked for `x86_64-pc-windows-gnu` and in CI, but **never run**. |
| **macOS** | ⚠️ **UNVERIFIED — needs testing on real hardware** | `NSOpenGLContext`-backed `NSView` inserted below the `WKWebView`. Compiled in CI for arm64 and x86_64, **never run**. Please test on both architectures: the Objective-C struct-return ABI differs between them. |

Be blunt about what that means: only the Linux backend has been executed. Windows and macOS are
carefully written and type-checked, and that is all. Reports and fixes very welcome.

## Requirements

- Tauri **v2**
- mpv **0.33+** (libmpv client API 1.107+) installed at runtime
  - Debian/Ubuntu: `apt install libmpv2` (or `libmpv-dev` to develop against it)
  - macOS: `brew install mpv`
  - Windows: ship `libmpv-2.dll` next to your executable, or set `TAURI_PLUGIN_MPV_LIBMPV_PATH`

Set `TAURI_PLUGIN_MPV_LIBMPV_PATH` to point at a specific library if auto-discovery misses it.

## Install

```bash
cargo add tauri-plugin-mpv     # see "Naming" below
npm install tauri-plugin-mpv-api
```

```rust
tauri::Builder::default()
    .plugin(tauri_plugin_mpv::init())
```

Add the permission to your capability file:

```json
{ "permissions": ["mpv:default"] }
```

### Window transparency

The video sits behind the webview, so the webview has to be see-through where the video is.

- **Linux** — nothing to configure. The plugin sets the WebKit webview's background to
  transparent itself. The *window* stays opaque, so no compositing window manager is required.
- **Windows and macOS** — set `"transparent": true` on the window in `tauri.conf.json`, which is
  what makes wry configure WebView2/WKWebView for transparency. On macOS this additionally
  requires Tauri's `macos-private-api` feature, which has App Store implications.

Then punch a hole in your page wherever the video should show:

```css
html, body { background: transparent; }
#player   { background: transparent; }   /* the element MpvVideo tracks */
```

Anything the page does not paint shows the video (or the window background) behind it, so paint
your chrome — control bars, panels — explicitly.

## API

### `MpvVideo` — the `<video>`-shaped API

| Member | Behaviour |
|---|---|
| `play()` / `pause()` | Sets mpv's `pause` property |
| `src` (get/set) | Setting it issues `loadfile`, as the real element begins loading |
| `load()` | Re-issues `loadfile` for the current `src` |
| `currentTime` (get/set) | Reads `time-pos`; setting issues `seek <t> absolute` |
| `duration`, `paused`, `ended`, `seeking` | Mirrored mpv state, read synchronously |
| `volume` (get/set) | **0..1**, converted to mpv's 0..100 |
| `muted`, `playbackRate` | mpv `mute`, `speed` |
| `videoWidth` / `videoHeight` | mpv `width` / `height` |
| `on*` handlers and `addEventListener` | `play`, `pause`, `timeupdate`, `durationchange`, `loadedmetadata`, `canplay`, `ended`, `seeking`, `seeked`, `volumechange`, `ratechange`, `error` |
| `attachTo(el)` / `detach()` | Track a DOM element's rect so the native surface follows it |
| `destroy()` | Tear down the surface and the mpv instance |

### Raw mpv API

Everything mpv can do that `<video>` has no vocabulary for:

```ts
import { command, setProperty, getProperty, observeProperty, onMpvEvent } from 'tauri-plugin-mpv-api'

await command(['loadfile', url, 'replace'])
await setProperty('sub-visibility', false)
const tracks = await getProperty('track-list')
await observeProperty('demuxer-cache-duration')
const unlisten = await onMpvEvent((e) => console.log(e))
```

### Configuration

```ts
await MpvVideo.create({
  options: { hwdec: 'auto-safe', 'sub-auto': 'fuzzy' },  // applied before mpv_initialize
  observe: ['demuxer-cache-duration'],
  logLevel: 'warn',      // mpv log level, forwarded as `log-message` events
  flipY: undefined,      // set true if video renders upside down (see below)
  advancedControl: false // MPV_RENDER_PARAM_ADVANCED_CONTROL
})
```

`vo` is ignored — the render API requires `vo=libmpv`.

`flipY` defaults to `true` on Linux and `false` on Windows and macOS.

The Linux value is **verified on screen**: `GtkGLArea` hands mpv a framebuffer whose rows run
opposite to mpv's default expectation, so without the flip the frame arrives upside down. The e2e
test plays a red-over-blue clip and asserts which half ends up where, because a symmetric test
pattern cannot tell a correct frame from a flipped one.

Windows and macOS render into the window's own default framebuffer rather than an offscreen FBO,
where OpenGL's bottom-left origin already matches mpv — so `false`. That is reasoned, not
observed. If video appears upside down there, set `flipY: true` and please open an issue.

## How it works

```
GtkOverlay  /  NSWindow contentView  /  parent HWND
├── overlay child : WebKitWebView / WKWebView / WebView2   (transparent, receives input)
└── main child    : GtkGLArea / NSView+NSOpenGL / child HWND+WGL
                     └── mpv_render_context_render() -> this framebuffer
```

The frontend reports its target element's rect each animation frame (only sending IPC when it
actually changes) and the native surface is moved to match, so the video appears to live inside a
DOM element while really being a GPU surface behind the glass.

`render.h` requires every `mpv_render_*` call to happen on the thread holding the GL context.
`RenderContext` is therefore `!Send` and `!Sync`, which is why it is *not* stored in the plugin's
shared state — only the `MpvCore` is. The surface owns the render context on the UI thread, and
mpv's update callback does nothing but set a flag for that thread to notice.

## Development

```bash
npm install && npm run build          # build the JS API
cargo test --lib                      # node/JSON conversion, geometry math
npx vitest run                        # the MpvVideo state machine
cargo build -p mpv-basic-player       # the example app
python3 tests/e2e/verify_linux.py     # headless compositing proof (Linux)
```

`tests/e2e/verify_linux.py` runs the example app under Xvfb and asserts on real pixels: that the
webview paints, that the video surface occupies the requested rect, that an HTML badge composites
*over* the video, and that a red-over-blue clip comes out the right way up. See
[`tests/e2e/README.md`](tests/e2e/README.md).

## Naming

`tauri-plugin-mpv` and `tauri-plugin-libmpv` are both taken on crates.io. This crate is
`publish = false` until a published name is settled; see the `TODO(publish-name)` in `Cargo.toml`.

## License

MIT OR Apache-2.0
