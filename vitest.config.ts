import { defineConfig } from 'vitest/config'

export default defineConfig({
  test: {
    // `cargo package` copies the whole crate — tests included — into target/, where vitest would
    // otherwise collect them a second time and cheerfully report double the test count.
    exclude: ['**/node_modules/**', '**/dist-js/**', 'target/**'],
  },
})
