import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],

  // Tauri drives this dev server and expects it exactly here, so a port
  // collision must fail loudly rather than silently move to 1421 and leave
  // the app pointed at nothing.
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      // Rust rebuilds are cargo's business; watching them here only produces
      // spurious frontend reloads during a build.
      ignored: ["**/src-tauri/**"],
    },
  },

  // Vite's own screen-clearing hides cargo errors that scrolled past.
  clearScreen: false,
});
