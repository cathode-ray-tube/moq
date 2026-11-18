import { defineConfig } from "vite";
import { worklet } from "../../common/vite-plugin-worklet";

export default defineConfig({
  plugins: [worklet()],
  optimizeDeps: {
    exclude: ["@cathode-ray-tube/moq-secure-publish"],
  },
});
