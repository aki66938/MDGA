/// <reference types="vitest/config" />
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react()],
  server: {
    port: 1420,
    strictPort: true
  },
  clearScreen: false,
  // 0.0.72：前端测试需 DOM（App.test.tsx 走 @testing-library/react）。此前缺 test 块，
  // vitest 默认 node 环境致 App.test.tsx 在 CI（npm test = vitest run）实际为红、测试门形同虚设。
  // jsdom 已是 devDependency，补配后测试门真正生效。
  test: {
    environment: "jsdom"
  }
});
