// Maps @tauri-apps/api imports onto the globals exposed by `withGlobalTauri`, so this example
// needs no bundler. A real app would import from @tauri-apps/api directly.
export const invoke = (...args) => window.__TAURI__.core.invoke(...args)
