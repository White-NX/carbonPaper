import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

const REACT_PACKAGES = new Set([
  "react",
  "react-dom",
  "scheduler",
  "react-i18next",
]);

const I18N_PACKAGES = new Set([
  "i18next",
  "i18next-browser-languagedetector",
]);

function getPackageName(id: string): string | null {
  const normalized = id.replace(/\\/g, "/");
  const marker = "/node_modules/";
  const start = normalized.lastIndexOf(marker);
  if (start === -1) {
    return null;
  }

  const modulePath = normalized.slice(start + marker.length);
  const parts = modulePath.split("/");
  if (!parts[0]) {
    return null;
  }

  return parts[0].startsWith("@") ? `${parts[0]}/${parts[1]}` : parts[0];
}

// https://vitejs.dev/config/
export default defineConfig(async () => ({
  plugins: [react()],
  build: {
    rollupOptions: {
      output: {
        manualChunks(id: string) {
          const pkg = getPackageName(id);
          if (!pkg) {
            return;
          }

          if (pkg.startsWith("@tauri-apps/")) {
            return "vendor-tauri";
          }

          if (REACT_PACKAGES.has(pkg)) {
            return "vendor-react";
          }

          if (I18N_PACKAGES.has(pkg)) {
            return "vendor-i18n";
          }
        },
      },
    },
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
