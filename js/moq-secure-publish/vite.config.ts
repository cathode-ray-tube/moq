import { resolve } from "path";
import { defineConfig } from "vite";
import { worklet } from "../common/vite-plugin-worklet";

export default defineConfig({
  plugins: [worklet()],
  resolve: {
    alias: {
      "@moq/net": resolve(__dirname, "../net/dist/index.js"),
    },
  },
  build: {
  lib: {
    entry: resolve(__dirname, "src/index.ts"),
    formats: ["es"],
    fileName: "index",
  },
  sourcemap: true,
  target: "esnext",
},
});
