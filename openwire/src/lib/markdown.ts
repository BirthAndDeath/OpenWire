import { fromMarkdown } from "mdast-util-from-markdown";
import { toHast } from "mdast-util-to-hast";
import { sanitize } from "hast-util-sanitize";
import { toHtml } from "hast-util-to-html";

const CACHE_MAX = 1000;
const CACHE_MAX_INPUT = 4 * 1024;
const cache = new Map<string, string>();

function escapeHtml(s: string): string {
    return s.replace(/[&<>"']/g, (c) =>
        c === "&" ? "&amp;" : c === "<" ? "&lt;" : c === ">" ? "&gt;" : c === '"' ? "&quot;" : "&#39;",
    );
}

interface HNode {
    type: string;
    tagName?: string;
    properties?: Record<string, unknown>;
    value?: string;
    children?: HNode[];
}

// 防误导性链接 + 协议白名单：
// 1) 外部链接可见文本与真实 href 不一致时，追加弱化小字显示真实目标；
// 2) 非 http(s)/mailto 的 href（相对路径、伪协议等）去掉跳转，防止消息内容把
//    应用导航到站内页面或触发未知协议。
// 使用显式栈迭代而非递归：消息文本来自对端，深度嵌套（大量嵌套引用/链接）时
// 递归会耗尽调用栈（栈溢出），迭代遍历不受输入深度影响。
function exposeMisleadingLinks(root: HNode): void {
    const stack: HNode[] = [root];
    while (stack.length > 0) {
        const node = stack.pop()!;
        if (
            node.type === "element" &&
            node.tagName === "a" &&
            node.properties &&
            typeof node.properties.href === "string"
        ) {
            const href = node.properties.href;
            if (!/^(https?:|mailto:)/i.test(href)) {
                // 协议白名单外：去除 href，链接退化为纯文本，不产生导航
                delete node.properties.href;
            } else if (/^https?:\/\//i.test(href)) {
                const text = (node.children ?? [])
                    .filter((c) => c.type === "text")
                    .map((c) => c.value ?? "")
                    .join("")
                    .trim();
                if (text !== href) {
                    node.children = node.children ?? [];
                    node.children.push({
                        type: "element",
                        tagName: "span",
                        properties: { className: ["md-link-url"] },
                        children: [{ type: "text", value: ` (${href})` }],
                    });
                }
            }
        }
        for (const child of node.children ?? []) stack.push(child);
    }
}

// 渲染输入防御上限：入站消息大小不可信（本地输入 maxlength 不约束对端），
// 超限内容仅截断展示（不修改存储），避免大输入占用解析/渲染内存与 CPU。
const MAX_RENDER_INPUT = 256 * 1024;

function render(text: string): string {
    if (text.length > MAX_RENDER_INPUT) {
        return escapeHtml(
            `${text.slice(0, MAX_RENDER_INPUT)}\n\n…[内容过长，仅展示前 256KB]`,
        );
    }
    try {
        const mdast = fromMarkdown(text);
        const hast = toHast(mdast);
        if (!hast) return escapeHtml(text);
        const cleaned = sanitize(hast);
        exposeMisleadingLinks(cleaned as unknown as HNode);
        return toHtml(cleaned);
    } catch {
        return escapeHtml(text);
    }
}

export function renderMarkdown(text: string): string {
    if (text.length > CACHE_MAX_INPUT) return render(text);
    const hit = cache.get(text);
    if (hit !== undefined) {
        cache.delete(text);
        cache.set(text, hit);
        return hit;
    }
    const html = render(text);
    cache.set(text, html);
    if (cache.size > CACHE_MAX) {
        const oldest = cache.keys().next().value;
        if (oldest !== undefined) cache.delete(oldest);
    }
    return html;
}
