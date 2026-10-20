import { defineConfig } from "vite";

export default defineConfig({
  // Relative asset paths so the build can be hosted from any subpath.
  base: "./",
  build: { target: "es2022" },
  test: { environment: "node" },
});
