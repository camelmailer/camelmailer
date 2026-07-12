import { defineConfig } from "vitest/config"

// Vitest for the client-side helper units. The crash-prone logic on the
// credentials page lives in pure helpers (maskKey / deriveSmtpHost /
// relativeTime), so they are unit-tested directly here. `tsconfigPaths`
// wires up the `@/` alias the source imports use (Vite reads it from
// tsconfig.json natively).
export default defineConfig({
  resolve: {
    tsconfigPaths: true,
  },
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
})
