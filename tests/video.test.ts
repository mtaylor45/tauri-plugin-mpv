import { describe, expect, it, beforeEach, vi } from 'vitest'
import { MpvVideo } from '../guest-js/video'
import type { MpvBackend, MpvEvent, MpvConfig, VideoRect } from '../guest-js/api'

/** Records every backend call so tests can assert on the exact mpv commands produced. */
class FakeBackend implements MpvBackend {
  calls: Array<{ fn: string; args: unknown[] }> = []
  rects: VideoRect[] = []
  observed: string[] = []
  private handler: ((e: MpvEvent) => void) | null = null

  private record(fn: string, ...args: unknown[]) {
    this.calls.push({ fn, args })
  }

  async init(config?: MpvConfig) {
    this.record('init', config)
    this.observed.push(...(config?.observe ?? []))
  }
  async destroy() {
    this.record('destroy')
  }
  async command(args: unknown[]) {
    this.record('command', ...args)
    return null
  }
  async setProperty(name: string, value: unknown) {
    this.record('setProperty', name, value)
  }
  async getProperty<T>(name: string): Promise<T> {
    this.record('getProperty', name)
    return null as T
  }
  async observeProperty(name: string) {
    this.observed.push(name)
  }
  async setVideoRect(rect: VideoRect) {
    this.rects.push(rect)
  }
  async setSurfaceVisible(visible: boolean) {
    this.record('setSurfaceVisible', visible)
  }
  async onMpvEvent(handler: (e: MpvEvent) => void) {
    this.handler = handler
    return () => {
      this.handler = null
    }
  }

  emit(e: MpvEvent) {
    this.handler?.(e)
  }

  commandsNamed(name: string) {
    return this.calls.filter((c) => c.fn === 'command' && c.args[0] === name)
  }
  lastCall(fn: string) {
    return [...this.calls].reverse().find((c) => c.fn === fn)
  }
}

let backend: FakeBackend
let video: MpvVideo

beforeEach(async () => {
  backend = new FakeBackend()
  video = await MpvVideo.create({ backend })
})

describe('property mapping', () => {
  it('converts HTML volume (0..1) to mpv volume (0..100)', () => {
    video.volume = 0.5
    expect(backend.lastCall('setProperty')?.args).toEqual(['volume', 50])
    expect(video.volume).toBe(0.5)
  })

  it('clamps volume to the HTML range', () => {
    video.volume = 5
    expect(backend.lastCall('setProperty')?.args).toEqual(['volume', 100])
    video.volume = -1
    expect(backend.lastCall('setProperty')?.args).toEqual(['volume', 0])
  })

  it('reports mpv volume back on the HTML scale', () => {
    backend.emit({ event: 'property-change', name: 'volume', value: 80 })
    expect(video.volume).toBeCloseTo(0.8)
  })

  it('maps muted to mpv mute', () => {
    video.muted = true
    expect(backend.lastCall('setProperty')?.args).toEqual(['mute', true])
  })

  it('maps playbackRate to mpv speed and rejects nonsense', () => {
    video.playbackRate = 2
    expect(backend.lastCall('setProperty')?.args).toEqual(['speed', 2])
    video.playbackRate = 0
    expect(backend.lastCall('setProperty')?.args).toEqual(['speed', 2])
    video.playbackRate = NaN
    expect(backend.lastCall('setProperty')?.args).toEqual(['speed', 2])
  })
})

describe('loading and seeking', () => {
  it('setting src issues a loadfile', () => {
    video.src = 'http://example.test/a.mkv'
    expect(backend.commandsNamed('loadfile')[0].args).toEqual([
      'loadfile',
      'http://example.test/a.mkv',
      'replace',
    ])
    expect(video.src).toBe('http://example.test/a.mkv')
  })

  it('setting currentTime issues an absolute seek, not a property set', () => {
    video.currentTime = 42.5
    expect(backend.commandsNamed('seek')[0].args).toEqual(['seek', 42.5, 'absolute'])
    // Optimistically reflected so UI reading it straight back sees the target.
    expect(video.currentTime).toBe(42.5)
  })

  it('ignores a non-finite seek', () => {
    video.currentTime = NaN
    expect(backend.commandsNamed('seek')).toHaveLength(0)
  })

  it('removeAttribute("src") stops playback', () => {
    video.src = 'x.mkv'
    video.removeAttribute('src')
    expect(backend.commandsNamed('stop')).toHaveLength(1)
    expect(video.src).toBe('')
  })

  it('reports duration as NaN when mpv has none, like HTMLMediaElement', () => {
    backend.emit({ event: 'property-change', name: 'duration', value: null })
    expect(Number.isNaN(video.duration)).toBe(true)
    backend.emit({ event: 'property-change', name: 'duration', value: 120 })
    expect(video.duration).toBe(120)
  })
})

describe('event translation', () => {
  it('fires play and pause only on an actual transition', () => {
    const onPlay = vi.fn()
    const onPause = vi.fn()
    video.addEventListener('play', onPlay)
    video.addEventListener('pause', onPause)

    // Starts paused; a repeated `pause: true` must not re-fire.
    backend.emit({ event: 'property-change', name: 'pause', value: true })
    expect(onPause).not.toHaveBeenCalled()

    backend.emit({ event: 'property-change', name: 'pause', value: false })
    expect(onPlay).toHaveBeenCalledTimes(1)
    expect(video.paused).toBe(false)

    backend.emit({ event: 'property-change', name: 'pause', value: true })
    expect(onPause).toHaveBeenCalledTimes(1)
  })

  it('supports on* handler properties as well as addEventListener', () => {
    const viaProperty = vi.fn()
    video.ontimeupdate = viaProperty
    const viaListener = vi.fn()
    video.addEventListener('timeupdate', viaListener)

    backend.emit({ event: 'property-change', name: 'time-pos', value: 12 })

    expect(viaProperty).toHaveBeenCalledTimes(1)
    expect(viaListener).toHaveBeenCalledTimes(1)
    expect(video.currentTime).toBe(12)
  })

  it('fires ended on eof but not on a user stop', () => {
    const onEnded = vi.fn()
    video.addEventListener('ended', onEnded)

    backend.emit({ event: 'end-file', reason: 'stop' })
    expect(onEnded).not.toHaveBeenCalled()
    expect(video.ended).toBe(false)

    backend.emit({ event: 'end-file', reason: 'eof' })
    expect(onEnded).toHaveBeenCalledTimes(1)
    expect(video.ended).toBe(true)
    expect(video.paused).toBe(true)
  })

  it('fires error when mpv ends a file with an error', () => {
    const onError = vi.fn()
    video.onerror = onError
    backend.emit({ event: 'end-file', reason: 'error' })
    expect(onError).toHaveBeenCalledTimes(1)
  })

  it('pairs seeking with seeked', () => {
    const onSeeking = vi.fn()
    const onSeeked = vi.fn()
    video.addEventListener('seeking', onSeeking)
    video.addEventListener('seeked', onSeeked)

    // A playback-restart with no preceding seek must not produce a bare `seeked`.
    backend.emit({ event: 'playback-restart' })
    expect(onSeeked).not.toHaveBeenCalled()

    backend.emit({ event: 'seek' })
    expect(onSeeking).toHaveBeenCalledTimes(1)
    expect(video.seeking).toBe(true)

    backend.emit({ event: 'playback-restart' })
    expect(onSeeked).toHaveBeenCalledTimes(1)
    expect(video.seeking).toBe(false)
  })

  it('clears ended when a new file loads', () => {
    backend.emit({ event: 'end-file', reason: 'eof' })
    expect(video.ended).toBe(true)
    backend.emit({ event: 'file-loaded' })
    expect(video.ended).toBe(false)
  })

  it('exposes video dimensions', () => {
    backend.emit({ event: 'property-change', name: 'width', value: 1920 })
    backend.emit({ event: 'property-change', name: 'height', value: 1080 })
    expect(video.videoWidth).toBe(1920)
    expect(video.videoHeight).toBe(1080)
  })

  it('observes the properties its mirrored state depends on', () => {
    for (const name of ['time-pos', 'duration', 'pause', 'volume', 'mute', 'speed']) {
      expect(backend.observed).toContain(name)
    }
  })
})

describe('geometry sync', () => {
  function fakeElement(rect: Partial<DOMRect>): HTMLElement {
    return {
      getBoundingClientRect: () => ({ left: 0, top: 0, width: 0, height: 0, ...rect }) as DOMRect,
    } as unknown as HTMLElement
  }

  it('sends the element rect and does not repeat an unchanged one', async () => {
    // Drive the rAF loop manually so the test is deterministic.
    const frames: FrameRequestCallback[] = []
    vi.stubGlobal('requestAnimationFrame', (cb: FrameRequestCallback) => {
      frames.push(cb)
      return frames.length
    })
    vi.stubGlobal('cancelAnimationFrame', () => {})

    video.attachTo(fakeElement({ left: 10, top: 20, width: 640, height: 360 }))
    frames.shift()!(0)
    expect(backend.rects).toEqual([{ x: 10, y: 20, width: 640, height: 360 }])

    // Same rect on the next frame: no second IPC call.
    frames.shift()!(1)
    expect(backend.rects).toHaveLength(1)

    vi.unstubAllGlobals()
  })

  it('syncGeometry pushes the current rect immediately', async () => {
    vi.stubGlobal('requestAnimationFrame', () => 1)
    vi.stubGlobal('cancelAnimationFrame', () => {})
    video.attachTo(fakeElement({ left: 5, top: 5, width: 100, height: 50 }))
    await video.syncGeometry()
    expect(backend.rects.at(-1)).toEqual({ x: 5, y: 5, width: 100, height: 50 })
    vi.unstubAllGlobals()
  })
})
