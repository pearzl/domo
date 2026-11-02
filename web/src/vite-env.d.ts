/// Vite 的 ?raw 导入返回文件原文，用于源码约束测试。
declare module "*?raw" {
  const content: string;
  export default content;
}

/// 测试在 Node 环境读取源码原文（样式约束断言），此处只声明用到的最小能力；
/// 项目未引入 @types/node，避免为一条只读用法扩大依赖面。
declare module "node:fs" {
  export function readFileSync(path: string, encoding: "utf8"): string;
}
