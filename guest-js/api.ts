/**
 * Raw mpv API. Thin wrappers over the plugin's Tauri commands.
 *
 * Most apps should prefer `MpvVideo`, which presents this as an `HTMLVideoElement`. This layer
 * is the escape hatch for everything mpv can do that `<video>` has no vocabulary for.
 */
import { invoke } from '@tauri-apps/api/core'
import { getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow'
import type { UnlistenFn } from '@tauri-apps/api/event'

export const EVENT_NAME = 'mpv-surface:event'

export interface MpvConfig {
  /** mpv options applied before initialization, e.g. `{ hwdec: 'auto-safe' }`. */
  options?: Record<string, string>
  /** Properties to observe immediately. */
  observe?: string[]
  /** Override framebuffer vertical orientation. Only set this if video renders upside down. */
  flipY?: boolean
  /** mpv log level: no, fatal, error, warn, info, v, debug, trace. Defaults to warn. */
  logLevel?: string
  /**
   * Enable MPV_RENDER_PARAM_ADVANCED_CONTROL. Off by default: it permits direct rendering and
   * makes render-thread violations fatal rather than silently degraded, but requires the host to
   * poll mpv for frame readiness.
   */
  advancedControl?: boolean
}

export interface VideoRect {
  x: number
  y: number
  width: number
  height: number
}

export type MpvEvent =
  | { event: 'property-change'; name: string; value: unknown }
  | { event: 'start-file' }
  | { event: 'file-loaded' }
  | { event: 'end-file'; reason: 'eof' | 'stop' | 'quit' | 'error' | 'unknown' }
  | { event: 'seek' }
  | { event: 'playback-restart' }
  | { event: 'shutdown' }
  | { event: 'log-message'; prefix: string; level: string; text: string }

/** Structured error from the plugin. `kind` is stable; branch on it rather than the message. */
export interface MpvError {
  kind:
    | 'MpvNotFound'
    | 'MpvSymbolMissing'
    | 'MpvTooOld'
    | 'Mpv'
    | 'NotInitialized'
    | 'AlreadyInitialized'
    | 'NoSuchWindow'
    | 'Surface'
    | 'RenderInit'
    | 'UnsupportedPlatform'
    | 'UnsupportedFormat'
    | 'InvalidArgument'
    | 'Tauri'
  message: string
}

export async function init(config?: MpvConfig): Promise<void> {
  await invoke('plugin:mpv-surface|init', { config })
}

export async function destroy(): Promise<void> {
  await invoke('plugin:mpv-surface|destroy')
}

export async function command(args: unknown[]): Promise<unknown> {
  return invoke('plugin:mpv-surface|command', { args })
}

export async function setProperty(name: string, value: unknown): Promise<void> {
  await invoke('plugin:mpv-surface|set_property', { name, value })
}

export async function getProperty<T = unknown>(name: string): Promise<T> {
  return invoke<T>('plugin:mpv-surface|get_property', { name })
}

export async function observeProperty(name: string): Promise<void> {
  await invoke('plugin:mpv-surface|observe_property', { name })
}

export async function unobserveProperty(name: string): Promise<void> {
  await invoke('plugin:mpv-surface|unobserve_property', { name })
}

export async function setVideoRect(rect: VideoRect): Promise<void> {
  await invoke('plugin:mpv-surface|set_video_rect', { rect })
}

export async function setSurfaceVisible(visible: boolean): Promise<void> {
  await invoke('plugin:mpv-surface|set_surface_visible', { visible })
}

export async function onMpvEvent(handler: (event: MpvEvent) => void): Promise<UnlistenFn> {
  return getCurrentWebviewWindow().listen<MpvEvent>(EVENT_NAME, (e) => handler(e.payload))
}

/** The backend `MpvVideo` talks to. Swappable so the facade can be tested without Tauri. */
export interface MpvBackend {
  init(config?: MpvConfig): Promise<void>
  destroy(): Promise<void>
  command(args: unknown[]): Promise<unknown>
  setProperty(name: string, value: unknown): Promise<void>
  getProperty<T = unknown>(name: string): Promise<T>
  observeProperty(name: string): Promise<void>
  setVideoRect(rect: VideoRect): Promise<void>
  setSurfaceVisible(visible: boolean): Promise<void>
  onMpvEvent(handler: (event: MpvEvent) => void): Promise<UnlistenFn>
}

export const tauriBackend: MpvBackend = {
  init,
  destroy,
  command,
  setProperty,
  getProperty,
  observeProperty,
  setVideoRect,
  setSurfaceVisible,
  onMpvEvent,
}
