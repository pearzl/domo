import { defineConfig, mergeConfig } from "vitest/config";
import viteConfig from "./vite.config";

/// 测试运行在 Node 环境，不加载 DOM；tsx 用例用 react-dom/server 渲染成静态标记。
export default mergeConfig(
  viteConfig,
  defineConfig({
    test: {
      environment: "node",
      include: ["src/**/*.test.ts", "src/**/*.test.tsx"],
    },
  }),
);
