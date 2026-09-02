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
| The top half is red and the bottom half is blue | Correct colour *and* vertical orientation (see below) |

## Orientation, and why the clip is asymmetric

The clip is solid **red over solid blue**, and the test asserts which half lands where. A
symmetric pattern would pass whether or not the framebuffer is vertically flipped, and `FLIP_Y` is
the single easiest thing to get wrong in a render-API integration.

This is not hypothetical: it caught a real bug. `GtkGLArea` hands mpv a framebuffer whose rows run
opposite to mpv's default expectation, so `flip_y` must be **`true`** on Linux. The value was
originally reasoned to be `false` from the toolkit's documented convention, and that reasoning was
wrong. Only the on-screen check settled it.

## When the colour check skips

mpv's OpenGL renderer emits near-black on some software GL stacks. That is reproducible with stock
mpv and has nothing to do with this plugin:

```
$ mpv --vo=gpu --gpu-sw=yes testpattern.mp4     # under Xvfb + llvmpipe
[vo/gpu/opengl] Suspected software renderer or indirect context.
VO: [gpu] 320x240 yuv420p
# ...renders (0,0,0) and (1,1,1) — near-black, though mpv's own logs show it correctly
# configured (Video source: 320x240, Video display: 320x240 -> 400x300) and its CPU path
# (--vo=image) decodes the clip to the right red and blue.
```

So the test decides on the *pixels*, not on the renderer's name: if both halves of the video rect
come back near-black it skips the colour check with that reason, and otherwise asserts. Sizing the
virtual display larger than the window (1024x768 for an 800x600 window) is what makes mpv render
in full colour under llvmpipe here — with the window exactly filling the screen it went black.

Force the check even when the output is near-black, to see it fail loudly:

```bash
python3 -u tests/e2e/verify_linux.py --strict-colours
```

## Environment notes

**WebKitGTK's headless behaviour is not consistent between environments.**
`WEBKIT_DISABLE_COMPOSITING_MODE` and `WEBKIT_DISABLE_DMABUF_RENDERER` are the usual workarounds,
and they cut both ways: they are what some images need in order to paint at all, and setting them
is what stops this project's dev container painting at all. The test therefore runs without them,
checks whether the webview actually painted, and retries once with them — reporting which mode it
used rather than leaving the difference mysterious:

```
attempt [default]: webview painted; using this run
```

The CI job is pinned to `ubuntu-24.04`, the release the Linux backend was verified on. On
`ubuntu-22.04` (libmpv 1.x, Mesa 23) the webview painted nothing at all and every sampled pixel
came back black — which is also why the "webview paints its own chrome" check asserts the control
bar's *actual colour* rather than merely "not the window background": the weaker assertion passes
on a completely black window, which is the exact failure it needs to catch.

**Waiting is on a signal, not a clock.** Creating mpv's render context compiles its GLSL, and a
software rasteriser can spend the better part of a minute on that. The test waits for mpv to
report a rendered frame rather than sleeping a fixed interval; sleeping screenshotted a
half-initialised app and produced confusing failures.
