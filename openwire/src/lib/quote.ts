// 引用消息的不可见内部标记。
// 组合阶段：[ ](quote:{源消息本地 id})；消息入列（Messagelist.add）时据此重写为
// 最终相对游标 [](quote:{k})。渲染/复制/联系人预览一律经 stripAllQuoteRefs，
// 避免内部标记泄漏到用户可见内容。
export const QUOTE_MARKER_RE = /\[\]\(quote:([^)]+)\)\s*$/;

export function hasQuoteRef(content: string): boolean {
    return QUOTE_MARKER_RE.test(content);
}

// 剥掉全部引用标记（嵌套引用的内部标记会累计，复制/渲染/再引用时须全量剥除）
export function stripAllQuoteRefs(content: string): string {
    return content.replace(/\[\]\(quote:[^)]*\)/g, "");
}

// 从引用消息内容还原被引用文本：去掉所有标记与 "> " 前缀。
// 内容匹配是跨端/跨重载场景下唯一稳定的引用依据（本地 id 在对方或重载后不成立），
// 仅作为游标/ id 无法定位时的兜底。
export function quotePlainText(content: string): string {
    return stripAllQuoteRefs(content)
        .split("\n")
        .map((line) => line.replace(/^(>\s?)+/, ""))
        .join("\n")
        .trim();
}

// 标记指向的是最终相对游标（纯整数）而非源消息 id
export function isQuoteOffset(ref: string): boolean {
    return /^-?\d+$/.test(ref);
}