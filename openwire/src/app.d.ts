/// <reference types="@sveltejs/kit" />

// 构建期 `?raw` 文本导入（如 About 页内嵌仓库根 README.md）
declare module "*.md?raw" {
    const content: string;
    export default content;
}