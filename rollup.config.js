import typescript from '@rollup/plugin-typescript'

const external = [/^@tauri-apps\/api/]

export default [
  {
    input: 'guest-js/index.ts',
    external,
    output: [
      { file: 'dist-js/index.js', format: 'esm' },
      { file: 'dist-js/index.cjs', format: 'cjs' },
    ],
    plugins: [typescript({ declaration: true, declarationDir: 'dist-js', rootDir: 'guest-js' })],
  },
  {
    // Same bundle for the example app, which resolves the Tauri API via import-map shims so it
    // needs no bundler of its own.
    input: 'guest-js/index.ts',
    external,
    output: {
      file: 'examples/basic-player/src/mpv-api.js',
      format: 'esm',
      paths: {
        '@tauri-apps/api/core': './shims/core.js',
        '@tauri-apps/api/event': './shims/event.js',
        '@tauri-apps/api/webviewWindow': './shims/webviewWindow.js',
      },
    },
    plugins: [typescript({ declaration: false, rootDir: 'guest-js' })],
  },
]
