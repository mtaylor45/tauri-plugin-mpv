import { MpvVideo } from './mpv-api.js'
import { invoke } from '@tauri-apps/api/core'

const $ = (id) => document.getElementById(id)

function fail(message) {
  const box = $('error')
  box.textContent = message
  box.style.display = 'block'
  document.title = 'mpv error'
}

async function main() {
  await invoke('frontend_ready', { stage: 'script running' }).catch(() => {})

  let video
  try {
    video = await MpvVideo.create({
      target: $('video'),
      // Software decode keeps this runnable headlessly; drop it to get hardware decode.
      // gpu-sw lets mpv render on a software GL stack (llvmpipe), which is what CI has.
      // Harmless on a real GPU. Drop `hwdec: 'no'` to get hardware decoding.
      options: { hwdec: 'no', 'loop-file': 'inf', 'gpu-sw': 'yes' },
      logLevel: 'info',
    })
  } catch (e) {
    fail(`${e.kind ?? 'Error'}: ${e.message ?? e}`)
    await invoke('frontend_ready', { stage: `mpv init failed: ${e.message ?? e}` }).catch(() => {})
    return
  }
  await invoke('frontend_ready', { stage: 'mpv initialised' }).catch(() => {})

  // Expose for the end-to-end test to poke at.
  window.__mpvVideo = video

  video.ontimeupdate = () => {
    const t = video.currentTime.toFixed(1)
    const d = Number.isNaN(video.duration) ? '--' : video.duration.toFixed(1)
    $('status').textContent = `${t} / ${d}`
  }
  video.onloadedmetadata = () => {
    $('status').textContent = `loaded ${video.videoWidth}x${video.videoHeight}`
    window.__mpvLoaded = true
  }
  video.onerror = () => fail('mpv failed to play the file')

  $('play').onclick = () => video.play()
  $('pause').onclick = () => video.pause()
  $('back').onclick = () => (video.currentTime = Math.max(0, video.currentTime - 10))
  $('fwd').onclick = () => (video.currentTime = video.currentTime + 10)

  const path = await invoke('test_file')
  if (path) {
    video.src = path
    await video.play()
  } else {
    $('status').textContent = 'set MPV_TEST_FILE to auto-load a clip'
  }
}

main().catch((e) =>
  invoke('frontend_ready', { stage: `unhandled error: ${e}` }).catch(() => {}),
)
