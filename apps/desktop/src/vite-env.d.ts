/// <reference types="vite/client" />

// Vite `?raw` 导入声明（把文件原始内容作为字符串导入）。0.0.74 返工后已无消费者
//（导出改为展示 iframe 自身用 foreignObject，弃 html2canvas/?raw），保留此通用声明以备后用、
// 且使根 tsconfig.base.json（typecheck 用，未含 vite/client）与 vite build 都识别 `?raw` 模块。
declare module "*?raw" {
  const content: string;
  export default content;
}
