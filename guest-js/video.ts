/**
 * `MpvVideo` — an `HTMLVideoElement`-shaped facade over mpv.
 *
 * The point is drop-in compatibility: player UI already written against a `<video>` element
 * should work unchanged when handed one of these. That is what makes adopting a native decoder
 * a one-line change instead of a rewrite of your playback layer.
 *
 * Two unit conversions matter and are easy to get wrong:
 *   - HTML `volume` is 0..1; mpv's is 0..100.
 *   - HTML `currentTime =` is a seek; mpv needs an explicit `seek <t> absolute` command.
 */
import {
  tauriBackend,
  type MpvBackend,
  type MpvConfig,
  type MpvEvent,
  type VideoRect,
} from './api'

export interface MpvVideoOptions extends MpvConfig {
  /** Backend override. Only useful for tests. */
  backend?: MpvBackend
  /** Element the video surface should track. May also be set later via `attachTo`. */
  target?: HTMLElement
  /** Skip `init()`; use when something else already initialized the plugin for this window. */
  skipInit?: boolean
}

/** Properties the facade needs observed to keep its mirrored state current. */
const OBSERVED = [
  'time-pos',
  'duration',
  'pause',
  'volume',
  'mute',
  'speed',
  'width',
  'height',
] as const

type Handler = ((this: MpvVideo, ev: Event) => unknown) | null

export class MpvVideo extends EventTarget {
  #backend: MpvBackend
  #unlisten: (() => void) | null = null
  #destroyed = false

  // Mirrored mpv state, so the getters are synchronous like the real element's.
  #src = ''
  #currentTime = 0
  #duration = NaN
  #paused = true
  #volume = 1
  #muted = false
  #playbackRate = 1
  #videoWidth = 0
  #videoHeight = 0
  #ended = false
  #seeking = false

  // Geometry tracking
  #target: HTMLElement | null = null
  #rafId: number | null = null
  #lastRect: VideoRect | null = null

  private constructor(backend: MpvBackend) {
    super()
    this.#backend = backend
  }

  static async create(options: MpvVideoOptions = {}): Promise<MpvVideo> {
    const { backend = tauriBackend, target, skipInit, ...config } = options
    const video = new MpvVideo(backend)

    if (!skipInit) {
      await backend.init({
        ...config,
        observe: [...new Set([...OBSERVED, ...(config.observe ?? [])])],
      })
    } else {
      for (const name of OBSERVED) await backend.observeProperty(name)
    }

    video.#unlisten = await backend.onMpvEvent((e) => video.#handleEvent(e))
    if (target) video.attachTo(target)
    return video
  }

  // ---------------------------------------------------------------- events

  #handleEvent(e: MpvEvent): void {
    switch (e.event) {
      case 'property-change':
        this.#handleProperty(e.name, e.value)
        break
      case 'file-loaded':
        this.#ended = false
        this.#fire('loadedmetadata')
        this.#fire('canplay')
        break
      case 'seek':
        this.#seeking = true
        this.#fire('seeking')
        break
      case 'playback-restart':
        if (this.#seeking) {
          this.#seeking = false
          this.#fire('seeked')
        }
        break
      case 'end-file':
        if (e.reason === 'eof') {
          this.#ended = true
          this.#paused = true
          this.#fire('ended')
        } else if (e.reason === 'error') {
          this.#fire('error')
        }
        break
      case 'shutdown':
        this.#destroyed = true
        break
    }
  }

  #handleProperty(name: string, value: unknown): void {
    switch (name) {
      case 'time-pos':
        if (typeof value === 'number') {
          this.#currentTime = value
          this.#fire('timeupdate')
        }
        break
      case 'duration':
        // mpv reports null for streams with no known duration; HTML uses NaN.
        this.#duration = typeof value === 'number' ? value : NaN
        this.#fire('durationchange')
        break
      case 'pause': {
        if (typeof value !== 'boolean') break
        const wasPaused = this.#paused
        this.#paused = value
        if (wasPaused && !value) this.#fire('play')
        else if (!wasPaused && value) this.#fire('pause')
        break
      }
      case 'volume':
        if (typeof value === 'number') {
          this.#volume = value / 100
          this.#fire('volumechange')
        }
        break
      case 'mute':
        if (typeof value === 'boolean') {
          this.#muted = value
          this.#fire('volumechange')
        }
        break
      case 'speed':
        if (typeof value === 'number') {
          this.#playbackRate = value
          this.#fire('ratechange')
        }
        break
      case 'width':
        if (typeof value === 'number') this.#videoWidth = value
        break
      case 'height':
        if (typeof value === 'number') this.#videoHeight = value
        break
    }
  }

  #fire(type: string): void {
    const event = new Event(type)
    this.dispatchEvent(event)
    const handler = (this as unknown as Record<string, Handler>)[`on${type}`]
    if (typeof handler === 'function') handler.call(this, event)
  }

  // ------------------------------------------------- HTMLMediaElement API

  get src(): string {
    return this.#src
  }

  /** Setting `src` begins loading, matching how the real element behaves. */
  set src(value: string) {
    this.#src = value
    if (value) void this.load()
    else void this.#backend.command(['stop'])
  }

  async load(): Promise<void> {
    if (!this.#src) return
    this.#ended = false
    this.#duration = NaN
    await this.#backend.command(['loadfile', this.#src, 'replace'])
  }

  async play(): Promise<void> {
    this.#ended = false
    await this.#backend.setProperty('pause', false)
  }

  async pause(): Promise<void> {
    await this.#backend.setProperty('pause', true)
  }

  /** Only `src` is meaningful here; present because `<video>` consumers call it. */
  removeAttribute(name: string): void {
    if (name === 'src') {
      this.#src = ''
      void this.#backend.command(['stop'])
    }
  }

  get currentTime(): number {
    return this.#currentTime
  }

  set currentTime(value: number) {
    if (!Number.isFinite(value)) return
    // Optimistic local update so a UI reading it back immediately sees the seek target.
    this.#currentTime = value
    void this.#backend.command(['seek', value, 'absolute'])
  }

  get duration(): number {
    return this.#duration
  }

  get paused(): boolean {
    return this.#paused
  }

  get ended(): boolean {
    return this.#ended
  }

  get seeking(): boolean {
    return this.#seeking
  }

  get videoWidth(): number {
    return this.#videoWidth
  }

  get videoHeight(): number {
    return this.#videoHeight
  }

  get volume(): number {
    return this.#volume
  }

  /** HTML volume is 0..1; mpv's is 0..100. */
  set volume(value: number) {
    const clamped = Math.min(1, Math.max(0, value))
    this.#volume = clamped
    void this.#backend.setProperty('volume', clamped * 100)
  }

  get muted(): boolean {
    return this.#muted
  }

  set muted(value: boolean) {
    this.#muted = value
    void this.#backend.setProperty('mute', value)
  }

  get playbackRate(): number {
    return this.#playbackRate
  }

  set playbackRate(value: number) {
    if (!Number.isFinite(value) || value <= 0) return
    this.#playbackRate = value
    void this.#backend.setProperty('speed', value)
  }

  // on* handler properties, assigned by consumers written against `<video>`.
  onplay: Handler = null
  onpause: Handler = null
  ontimeupdate: Handler = null
  onended: Handler = null
  onloadedmetadata: Handler = null
  oncanplay: Handler = null
  ondurationchange: Handler = null
  onvolumechange: Handler = null
  onratechange: Handler = null
  onseeking: Handler = null
  onseeked: Handler = null
  onerror: Handler = null

  // ------------------------------------------------------------- geometry

  /**
   * Track `element`'s position so the native surface stays lined up with it.
   *
   * Polls on animation frames and only sends IPC when the rect actually changes. That covers
   * CSS transitions and layout shifts that `ResizeObserver` alone would miss, at the cost of one
   * `getBoundingClientRect()` per frame.
   */
  attachTo(element: HTMLElement): void {
    this.detach()
    this.#target = element
    const tick = () => {
      if (this.#target && !this.#destroyed) {
        const r = this.#target.getBoundingClientRect()
        const rect: VideoRect = { x: r.left, y: r.top, width: r.width, height: r.height }
        if (!rectsEqual(rect, this.#lastRect)) {
          this.#lastRect = rect
          void this.#backend.setVideoRect(rect)
        }
        this.#rafId = requestAnimationFrame(tick)
      }
    }
    this.#rafId = requestAnimationFrame(tick)
  }

  detach(): void {
    if (this.#rafId !== null) {
      cancelAnimationFrame(this.#rafId)
      this.#rafId = null
    }
    this.#target = null
    this.#lastRect = null
  }

  /** Force an immediate geometry sync. Rarely needed; the frame loop handles normal layout. */
  async syncGeometry(): Promise<void> {
    if (!this.#target) return
    const r = this.#target.getBoundingClientRect()
    const rect: VideoRect = { x: r.left, y: r.top, width: r.width, height: r.height }
    this.#lastRect = rect
    await this.#backend.setVideoRect(rect)
  }

  // -------------------------------------------------------------- teardown

  async destroy(): Promise<void> {
    if (this.#destroyed) return
    this.#destroyed = true
    this.detach()
    if (this.#unlisten) {
      this.#unlisten()
      this.#unlisten = null
    }
    await this.#backend.destroy()
  }
}

function rectsEqual(a: VideoRect, b: VideoRect | null): boolean {
  return (
    b !== null && a.x === b.x && a.y === b.y && a.width === b.width && a.height === b.height
  )
}
