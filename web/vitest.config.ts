import { defineConfig } from "vitest/config";

/// 表现层测试只覆盖纯函数，运行在 Node 环境，不加载 DOM。
export default defineConfig({
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});
