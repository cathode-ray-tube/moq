import { defineConfig } from "vite";
import { worklet } from "../../common/vite-plugin-worklet";

export default defineConfig({
  plugins: [worklet()],
  optimizeDeps: {
    exclude: ["@moq/publish", "@moq/net"],
  },
});
