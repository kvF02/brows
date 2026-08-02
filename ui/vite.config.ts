import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";

// The UI is shipped as static assets to the Tauri shell.  The dev-server
// allowances are intentionally limited to loopback and are not used in a
// production build.
export default defineConfig(({ command }) => {
  const productionCsp: Plugin = {
    name: "brows-production-csp",
    transformIndexHtml(html) {
      if (command !== "build") return html;
      return html.replace(
        "connect-src 'self' ipc: http://ipc.localhost http://127.0.0.1:4173 ws://127.0.0.1:4173",
        "connect-src 'self' ipc: http://ipc.localhost",
      );
    },
  };

  return {
    base: "./",
    plugins: [react(), productionCsp],
    server: {
      host: "127.0.0.1",
      port: 4173,
      strictPort: true,
    },
    build: {
      target: "es2022",
      sourcemap: false,
    },
  };
});
