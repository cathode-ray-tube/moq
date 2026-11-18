import { resolve } from "path";
import { defineConfig } from "vite";
import { worklet } from "../common/vite-plugin-worklet";

const hangDist = resolve(__dirname, "../hang/dist");

export default defineConfig({
  plugins: [worklet()],
  resolve: {
    alias: [
      {
        find: /^@moq\/hang$/,
        replacement: resolve(hangDist, "index.js"),
      },
      {
        find: /^@moq\/hang\/catalog$/,
        replacement: resolve(hangDist, "catalog/index.js"),
      },
      {
        find: /^@moq\/hang\/container$/,
        replacement: resolve(hangDist, "container/index.js"),
      },
      {
        find: /^@moq\/hang\/util$/,
        replacement: resolve(hangDist, "util/index.js"),
      },
      {
        find: "@moq/net",
        replacement: resolve(__dirname, "../net/dist/index.js"),
      },
    ],
  },
  build: {
    lib: {
      entry: resolve(__dirname, "src/index.ts"),
      formats: ["es"],
      fileName: "index",
    },
    rollupOptions: {
      // Keep other @moq workspace deps external so consumers can dedupe them.
      // @moq/hang is aliased to the local build above, so it gets bundled.
      external: (id) =>
        (id.startsWith("@moq/") && !id.startsWith("@moq/hang")) ||
        id === "media-captions",
    },
    sourcemap: true,
    target: "esnext",
  },
});
