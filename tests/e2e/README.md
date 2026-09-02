# End-to-end compositing test

`verify_linux.py` runs the example app against a headless X server and inspects real pixels.

```bash
cargo build -p mpv-basic-player
python3 -u tests/e2e/verify_linux.py
```

## What it asserts

| Check | Proves |
|---|---|
| The control bar is its own colour | The webview paints at all |
| The video rect is not the window background | The GL surface exists and mpv painted it, at the rect the frontend asked for |
| A badge inside the video rect is green | **HTML composites on top of the video** — the whole point of the plugin |
| The app log reports a loaded file and a rendered frame | mpv actually decoded and drove a frame through the render context |

## Why the colour check is skipped headlessly

The clip is deliberately asymmetric — solid red over solid blue — so that a vertically flipped
framebuffer is detectable. A symmetric pattern would pass either way, and `FLIP_Y` is the easiest
thing to get wrong in a GtkGLArea/mpv integration.

That colour check is skipped on a software GL stack, and not out of caution. **mpv's OpenGL
renderer produces near-black output under llvmpipe.** This is reproducible with stock mpv in the
same container and has nothing to do with this plugin:

```
$ mpv --vo=gpu --gpu-sw=yes testpattern.mp4     # under Xvfb + llvmpipe
[vo/gpu/opengl] Suspected software renderer or indirect context.
VO: [gpu] 320x240 yuv420p
# ...renders (0,0,0) and (1,1,1) — the same near-black signature
```

mpv's own logs confirm it is otherwise configured correctly in this situation
(`Video source: 320x240`, `Video display: 320x240 -> 400x300`), and mpv's CPU path (`--vo=image`)
decodes the clip to the correct red and blue. So the plugin's pipeline — GL context, framebuffer
targeting, geometry, compositing order — is verified; only mpv's own pixel output is unavailable
on a software rasteriser.

**This also means vertical orientation is unverified.** `flip_y` defaults to `false` on every
backend, reasoned from the toolkits' documented framebuffer conventions rather than observed on
screen. Sampling the region tints under llvmpipe is not a substitute: the same region returns
`(0,0,0)`, `(0,0,20)` and `(34,34,34)` across runs, so any orientation inferred from it is noise.
If video renders upside down on real hardware, set `flipY: true` and please open an issue so the
default can be corrected.

On a machine with a real GPU the colour checks run automatically. Force them anywhere with:

```bash
python3 -u tests/e2e/verify_linux.py --strict-colours
```

## Environment notes

Do **not** set `WEBKIT_DISABLE_COMPOSITING_MODE` or `WEBKIT_DISABLE_DMABUF_RENDERER`. They are the
usual headless-WebKit workarounds, but here they stop the webview painting entirely, which
silently defeats the compositing check — the test would still see video and report a pass on a
window with no UI in it.
