#!/usr/bin/env python3
"""
End-to-end proof that the Linux surface composites correctly.

Runs the example app against a headless X server and inspects the resulting pixels.

What this proves, always:

  1. the webview paints                      -> the control bar is its own colour
  2. the video surface exists and is painted -> the video rect is not the window background
  3. HTML composites ON TOP of the video     -> a badge inside the video rect is its own colour
  4. mpv actually decoded and rendered       -> the app log reports a loaded file and a frame

  5. the clip renders the right way up   -> red half on top, blue half below

Check 5 is what pins down FLIP_Y, and it is why the clip is asymmetric: a symmetric pattern
cannot tell a correct frame from a vertically flipped one. It is skipped only when mpv rendered
near-black, which some software GL stacks do (reproducible with stock `mpv --vo=gpu --gpu-sw=yes`
and nothing to do with this plugin). Force it anyway with --strict-colours.

The clip is deliberately asymmetric — solid red over solid blue — because a symmetric pattern
cannot distinguish correct output from a vertically flipped framebuffer, which is the single
easiest thing to get wrong in a GtkGLArea/mpv integration.

Requires: Xvfb, ImageMagick, xwd, ffmpeg, and libmpv at runtime.
"""

import argparse
import os
import re
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SCRATCH = Path(os.environ.get("MPV_E2E_DIR", "/tmp/tauri-plugin-mpv-e2e"))
DISPLAY = os.environ.get("MPV_E2E_DISPLAY", ":99")
WIDTH, HEIGHT = 800, 600
# Give the virtual display room around the window so its size is never clamped by the screen.
SCREEN_W, SCREEN_H = 1024, 768
# CI runners are slow: WebKitGTK's first initialisation alone can take tens of seconds.
WINDOW_TIMEOUT_S = 90

# Must match the CSS in examples/basic-player/src/index.html.
VIDEO = (100, 80, 400, 300)  # left, top, w, h
WINDOW_BG = (246, 245, 244)  # GTK's default window background, i.e. "nothing painted here"

BADGE_REGION = (132, 108, 36, 24)
BAR_REGION = (380, 560, 40, 20)
VIDEO_TOP = (250, 120, 100, 60)
VIDEO_BOTTOM = (250, 280, 100, 60)


def run(cmd, capture_output=True, text=True, **kw):
    if not capture_output:
        text = False
    return subprocess.run(cmd, capture_output=capture_output, text=text, **kw)


def need(tool):
    if not shutil.which(tool):
        sys.exit(f"FATAL: `{tool}` is required but not installed")


def make_clip(path: Path):
    """Solid red on top of solid blue, so a vertical flip is detectable."""
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        return
    r = run([
        "ffmpeg", "-y", "-loglevel", "error",
        "-f", "lavfi", "-i", "color=c=red:s=320x120:d=10:r=25",
        "-f", "lavfi", "-i", "color=c=blue:s=320x120:d=10:r=25",
        "-filter_complex", "[0:v][1:v]vstack=inputs=2[v]", "-map", "[v]",
        "-pix_fmt", "yuv420p", "-c:v", "libx264", str(path),
    ])
    if r.returncode != 0:
        sys.exit(f"FATAL: could not generate the test clip:\n{r.stderr}")


def window_tree():
    r = run(["xwininfo", "-root", "-tree", "-display", DISPLAY])
    return r.stdout if r.returncode == 0 else ""


def find_window():
    """Return (id, abs_x, abs_y) of the app window.

    Matched by WM_CLASS first — the window's size is not a reliable key, since a compositor or
    the display size can change it — falling back to an exactly-sized window.
    """
    candidates = []
    for line in window_tree().splitlines():
        m = re.search(r"(0x[0-9a-f]+)", line)
        if not m:
            continue
        wid = m.group(1)
        size = re.search(r"\s(\d+)x(\d+)\+", line)
        if "mpv-basic-player" in line:
            candidates.insert(0, wid)
        elif size and int(size.group(1)) == WIDTH and int(size.group(2)) == HEIGHT:
            candidates.append(wid)

    for wid in candidates:
        info = run(["xwininfo", "-display", DISPLAY, "-id", wid])
        if info.returncode != 0:
            continue
        ax = ay = None
        w = h = 0
        for il in info.stdout.splitlines():
            am = re.search(r"Absolute upper-left (X|Y):\s+(-?\d+)", il)
            if am:
                if am.group(1) == "X":
                    ax = int(am.group(2))
                else:
                    ay = int(am.group(2))
            wm = re.search(r"Width:\s+(\d+)", il)
            if wm:
                w = int(wm.group(1))
            hm = re.search(r"Height:\s+(\d+)", il)
            if hm:
                h = int(hm.group(1))
        # Skip tiny helper windows WebKit and GTK create alongside the real one.
        if ax is None or ay is None or w < WIDTH or h < HEIGHT:
            continue
        return wid, ax, ay
    return None


def screenshot(path: Path):
    """Capture the root window.

    `import -window <id>` grabs the X server and hangs when there is no window manager, so dump
    the root with xwd and convert instead.
    """
    xwd = run(["xwd", "-display", DISPLAY, "-root", "-silent"], capture_output=False,
              stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if xwd.returncode != 0 or not xwd.stdout:
        sys.exit("FATAL: xwd failed to capture the root window")
    conv = subprocess.run(["convert", "xwd:-", str(path)], input=xwd.stdout, capture_output=True)
    if conv.returncode != 0:
        sys.exit(f"FATAL: converting the xwd dump failed:\n{conv.stderr.decode()}")


def region_color(png: Path, region):
    x, y, w, h = region
    r = run([
        "convert", str(png), "-crop", f"{w}x{h}+{x}+{y}", "+repage",
        "-resize", "1x1!", "-depth", "8", "-format", "%[hex:p{0,0}]", "info:",
    ])
    if r.returncode != 0:
        sys.exit(f"FATAL: ImageMagick failed reading {region}:\n{r.stderr}")
    hexval = r.stdout.strip()[:6]
    return tuple(int(hexval[i:i + 2], 16) for i in (0, 2, 4))


def near_black(rgb, threshold=40):
    return max(rgb) < threshold


def close_to(a, b, tol=12):
    return all(abs(x - y) <= tol for x, y in zip(a, b))


def classify(rgb):
    r, g, b = rgb
    if r > 90 and r > g * 2 and r > b * 2:
        return "red"
    if g > 90 and g > r * 2 and g > b * 2:
        return "green"
    if b > 90 and b > r * 2 and b > g * 2:
        return "blue"
    return f"other{rgb}"


class Checks:
    def __init__(self):
        self.failures = []
        self.skipped = []

    def check(self, name, ok, detail):
        print(f"  {'PASS' if ok else 'FAIL'}  {name:<42} {detail}")
        if not ok:
            self.failures.append(name)

    def skip(self, name, why):
        print(f"  SKIP  {name:<42} {why}")
        self.skipped.append(name)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--strict-colours", "--strict-colors", dest="strict", action="store_true",
                    help="run the red/blue colour checks even on a software GL stack")
    args = ap.parse_args()

    for tool in ("Xvfb", "convert", "xwininfo", "xwd", "ffmpeg"):
        need(tool)

    binary = REPO / "target" / "debug" / "mpv-basic-player"
    if not binary.exists():
        sys.exit(f"FATAL: build the example first: cargo build -p mpv-basic-player\n({binary})")

    SCRATCH.mkdir(parents=True, exist_ok=True)
    clip = SCRATCH / "testpattern.mp4"
    make_clip(clip)
    shot = SCRATCH / "screenshot.png"
    log_path = SCRATCH / "app.log"

    xvfb = subprocess.Popen(
        ["Xvfb", DISPLAY, "-screen", "0", f"{SCREEN_W}x{SCREEN_H}x24", "-nolisten", "tcp"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    app = None
    try:
        time.sleep(1.5)
        if xvfb.poll() is not None:
            sys.exit(f"FATAL: Xvfb could not start on {DISPLAY} (is it already in use?)")

        env = {
            **os.environ,
            "DISPLAY": DISPLAY,
            "GDK_BACKEND": "x11",
            "MPV_TEST_FILE": str(clip),
            # llvmpipe: no GPU in CI or in a container.
            "LIBGL_ALWAYS_SOFTWARE": "1",
            # NB: do *not* set WEBKIT_DISABLE_COMPOSITING_MODE or WEBKIT_DISABLE_DMABUF_RENDERER.
            # They are the usual headless-WebKit workarounds, but here they stop the webview
            # painting at all, which silently defeats the compositing check.
        }
        with open(log_path, "w") as log:
            app = subprocess.Popen([str(binary)], env=env, stdout=log,
                                   stderr=subprocess.STDOUT)

            found = None
            deadline = time.monotonic() + WINDOW_TIMEOUT_S
            while time.monotonic() < deadline:
                time.sleep(0.5)
                if app.poll() is not None:
                    print(log_path.read_text()[-4000:])
                    sys.exit(f"FATAL: the app exited early with code {app.returncode}")
                found = find_window()
                if found:
                    break
            if not found:
                # Without these, a detection failure in CI is undiagnosable.
                print("--- xwininfo -root -tree ---")
                print(window_tree() or "(xwininfo produced no output)")
                print("--- app log ---")
                print(log_path.read_text()[-4000:] or "(empty)")
                sys.exit(
                    f"FATAL: no {WIDTH}x{HEIGHT} app window appeared within {WINDOW_TIMEOUT_S}s"
                )
            window, ox, oy = found

            time.sleep(6)
            if app.poll() is not None:
                print(log_path.read_text()[-4000:])
                sys.exit(f"FATAL: the app exited while waiting for a frame ({app.returncode})")

            screenshot(shot)

        app_log = log_path.read_text()
        print(f"window {window} at +{ox}+{oy}, screenshot -> {shot}")
        print()

        c = Checks()

        # 1. The webview renders at all.
        bar = region_color(shot, (BAR_REGION[0] + ox, BAR_REGION[1] + oy, *BAR_REGION[2:]))
        c.check("webview paints its own chrome", not close_to(bar, WINDOW_BG),
                f"control bar rgb={bar}")

        # 2. The GL surface exists where the frontend asked for it.
        top = region_color(shot, (VIDEO_TOP[0] + ox, VIDEO_TOP[1] + oy, *VIDEO_TOP[2:]))
        bottom = region_color(shot, (VIDEO_BOTTOM[0] + ox, VIDEO_BOTTOM[1] + oy,
                                     *VIDEO_BOTTOM[2:]))
        c.check("video surface occupies the target rect",
                not close_to(top, WINDOW_BG) and not close_to(bottom, WINDOW_BG),
                f"upper rgb={top} lower rgb={bottom}")

        # 3. HTML draws over the video — the whole point of the plugin.
        badge = region_color(shot, (BADGE_REGION[0] + ox, BADGE_REGION[1] + oy,
                                    *BADGE_REGION[2:]))
        c.check("HTML composites over the video", classify(badge) == "green",
                f"badge rgb={badge}")

        # 4. mpv really opened the file and drove a frame through the render context.
        c.check("mpv loaded the file", "file loaded" in app_log,
                "app log reports a loaded file")
        c.check("mpv rendered a frame", "first mpv frame rendered" in app_log,
                "app log reports a rendered frame")

        # 5. Colours, including orientation. Decided on the pixels themselves rather than the
        # renderer's name: some software stacks render the video fine and some emit near-black,
        # and only the latter makes the check meaningless.
        if near_black(top) and near_black(bottom) and not args.strict:
            c.skip("video colours and orientation",
                   f"mpv rendered near-black (upper={top} lower={bottom}); "
                   "stock mpv --vo=gpu does the same on this GL stack")
        else:
            c.check("video top half is red", classify(top) == "red", f"rgb={top}")
            c.check("video bottom half is blue", classify(bottom) == "blue", f"rgb={bottom}")

        print()
        if c.failures:
            print("--- app log ---")
            print(app_log[-4000:])
            print(f"\nFAILED: {', '.join(c.failures)}")
            return 1
        msg = "All checks passed"
        if c.skipped:
            msg += f" ({len(c.skipped)} skipped: {', '.join(c.skipped)})"
        print(msg + ".")
        return 0
    finally:
        for p in (app, xvfb):
            if p and p.poll() is None:
                p.send_signal(signal.SIGTERM)
                try:
                    p.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    p.kill()


if __name__ == "__main__":
    sys.exit(main())
