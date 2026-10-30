/// Vite 的 ?raw 导入返回文件原文，用于源码约束测试。
declare module "*?raw" {
  const content: string;
  export default content;
}
