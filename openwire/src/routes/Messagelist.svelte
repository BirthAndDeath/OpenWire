<script lang="ts">
    import { VList, type VListHandle } from "virtua/svelte";
    import { tick } from "svelte";
    import { invoke } from "@tauri-apps/api/core";
    import { _ } from "svelte-i18n";
    import { tt } from "../lib/i18n";
    import { fontSizeScale } from "../lib/settings";
    import { formatFileSize } from "../lib/format";
    import type { FileHashInfo } from "../lib/types";
    import { renderMarkdown } from "../lib/markdown";
    import {
        QUOTE_MARKER_RE,
        hasQuoteRef,
        isQuoteOffset,
        quotePlainText,
        stripAllQuoteRefs,
    } from "../lib/quote";
    import {
        transferTasks,
        removeTransfer,
        calcProgress,
    } from "../lib/transfers";
import type { FileTransferProgress } from "../lib/transfers";

    let fontSize = $derived($fontSizeScale * 13 + "px");

    interface Msg {
        id: string;
        content: string;
        ts: number;
        me: boolean;
        // 消息类型: text | file_hash | file_stream
        type: "text" | "file_hash" | "file_stream";
        // FileHash 相关
        file_hash_info?: FileHashInfo;
        // 发送方 ML-DSA 公钥 hex（用于点击 FileHash 时发起下载请求）
        sender_mldsa_pubkey_hex?: string;
        // 消息所属联系人的 ML-DSA 公钥 hex
        mldsa_pubkey_hex: string;
        // 消息发送状态: true = 待确认（未收到送达回执）, false = 已送达
        pending?: boolean;
        // 消息哈希（用于匹配送达回执）
        message_hash?: string;
    }

    // 文件传输进度统一读取全局存储 transferTasks（聊天卡片与全局面板同一权威来源）

    let {
        contactId = null,
        jumpTarget = null,
        onquote = () => {},
    }: {
        contactId: string | null;
        jumpTarget?: { peer: string; id: number } | null;
        /** 长按菜单"引用"回调：将引用文本（Markdown）交由父级插入输入框 */
        onquote?: (quoteText: string) => void;
    } = $props();

    let msgs = $state<Msg[]>([]);
    let vlist: VListHandle | undefined = $state();
    // 已加载的消息 ID 集合，用于去重
    let loadedMsgIds = $state<Set<string>>(new Set());
    // 去重集滑动容量上限：仅用于防重复 add，旧消息 id 不会重现；
    // 超出上限时按插入序淘汰最旧项，避免长会话内存线性增长
    const DEDUP_SET_MAX = 8192;
    function dedupAdd(id: string) {
        loadedMsgIds.add(id);
        if (loadedMsgIds.size > DEDUP_SET_MAX) {
            const oldest = loadedMsgIds.values().next().value;
            if (oldest !== undefined) loadedMsgIds.delete(oldest);
        }
    }
    // 跳转高亮行（消息 id 字符串）与防重复跳转守卫
    let highlightId = $state<string | null>(null);
    let lastJumpedId = -1;

    // 长按/右键消息弹出操作菜单（当前仅"复制"）
    let msgMenu = $state<{ x: number; y: number; msg: Msg } | null>(null);
    let longPressTimer: ReturnType<typeof setTimeout> | undefined = undefined;

    // 长按（触摸/手写笔）触发；鼠标左键不响应（由右键菜单覆盖同样入口）
    function handlePointerDown(e: PointerEvent, m: Msg) {
        if (e.pointerType !== "touch" && e.pointerType !== "pen") return;
        clearTimeout(longPressTimer);
        longPressTimer = setTimeout(() => {
            openMsgMenuAt(e.clientX, e.clientY, m);
        }, 480);
    }
    function clearLongPress() {
        clearTimeout(longPressTimer);
        longPressTimer = undefined;
    }

    function openMsgMenuAt(x: number, y: number, m: Msg) {
        const MENU_W = 160;
        const MENU_H = 96;
        msgMenu = {
            x: Math.min(Math.max(4, x), window.innerWidth - MENU_W - 4),
            y: Math.min(Math.max(4, y), window.innerHeight - MENU_H - 4),
            msg: m,
        };
    }

    // 引用：将消息内容转成 Markdown 引用块（每行加 "> "），末尾附带内部标记。
    // 组合阶段标记携带源消息的本地 id（[](quote:{id})），消息入列（add）时会在当前
    // 列表这一刻重写为最终相对游标（[](quote:{k})，见 finalizeQuoteRef），因此
    // 引用->发送之间到达的新消息不会让游标偏移。渲染/复制/联系人预览都会剥掉标记。
    function quoteMessage(m: Msg) {
        msgMenu = null;
        // 若原文本身是引用消息，先全量剥掉内部标记并折叠所有 "> " 层级再引用，
        // 避免引用套娃（>> 嵌套）与内部标记在内容里累计
        const raw = hasQuoteRef(m.content)
            ? stripAllQuoteRefs(m.content)
                  .split("\n")
                  .map((line) => line.replace(/^(>\s?)+/, ""))
                  .join("\n")
            : m.content;
        const quoted = raw
            .split("\n")
            .map((line) => `> ${line}`)
            .join("\n");
        onquote(`${quoted}[](quote:${m.id})\n\n\n`);
    }

    // 引用消息入列定稿：把组合阶段的 [](quote:{源id}) 重写为对当前列表的相对游标。
    // 源 id 可能因列表重载失效（临时 id 被换掉），此时按被引用文本在列表里找回源，
    // 保证线上携带的是正确的相对游标，也让发送方自己能跳转。
    function finalizeQuoteRef(content: string): string {
        const marker = content.match(QUOTE_MARKER_RE);
        if (marker && !isQuoteOffset(marker[1])) {
            let srcIdx = msgs.findIndex((x) => x.id === marker[1]);
            if (srcIdx < 0) {
                const target = quotePlainText(content);
                srcIdx = msgs.findIndex(
                    (x) => target !== "" && x.content.trim() === target,
                );
            }
            if (srcIdx >= 0) {
                // 引用消息即将 push 到尾部：其与源消息的距离 = 当前长度 - 源下标
                return content.replace(
                    QUOTE_MARKER_RE,
                    `[](quote:${msgs.length - srcIdx})`,
                );
            }
        }
        return content;
    }

    // 内容兜底：游标/id 都不可用（跨端 id 不同、本地重载后临时 id 失效）时，
    // 从引用消息所在位置向前按被引用文本找最近匹配；找不到就算了。
    function quoteTargetByContent(m: Msg): number | null {
        const target = quotePlainText(m.content);
        if (!target) return null;
        const quoteIdx = msgs.findIndex((x) => x.id === m.id);
        for (let i = quoteIdx - 1; i >= 0; i--) {
            if (msgs[i].content.trim() === target) return i;
        }
        // 兼容顺序轻微偏差：全窗口二次匹配
        for (let i = 0; i < msgs.length; i++) {
            if (i !== quoteIdx && msgs[i].content.trim() === target) return i;
        }
        return null;
    }

    // 解析引用目标索引：数字游标 → 源消息 id（含旧格式 peer:id）→ 内容兜底。
    // 越界/找不到则放弃（找不到就算了）。
    function quoteTargetOf(m: Msg): number | null {
        const marker = m.content.match(QUOTE_MARKER_RE);
        if (!marker) return null;
        const ref = marker[1];
        if (isQuoteOffset(ref)) {
            const quoteIdx = msgs.findIndex((x) => x.id === m.id);
            if (quoteIdx >= 0) {
                const target = quoteIdx - parseInt(ref, 10);
                if (target >= 0 && target < msgs.length) return target;
            }
            return quoteTargetByContent(m);
        }
        // 兼容旧格式 `peerHex:id` 与当前裸消息 id
        const id = ref.includes(":") ? ref.slice(ref.lastIndexOf(":") + 1) : ref;
        const idx = msgs.findIndex((x) => x.id === id);
        if (idx >= 0) return idx;
        return quoteTargetByContent(m);
    }

    // 点击引用气泡：定位到被引用消息并滚动+高亮。align=center 保证即使目标已在
    // 视口内也会产生可见跳转（auto 在目标可见时不移动，会被误判为"不跳转"）。
    function jumpToQuote(m: Msg) {
        const targetIdx = quoteTargetOf(m);
        if (targetIdx === null) return;
        vlist?.scrollToIndex(targetIdx, { align: "center", smooth: false });
        highlightId = msgs[targetIdx].id;
        setTimeout(() => (highlightId = null), 2500);
    }

    async function copyMessage(m: Msg) {
        msgMenu = null;
        // 引用消息复制时剥掉全部内部标记，避免粘贴出 [](quote)
        const raw =
            typeof m.content === "string" ? m.content : String(m.content ?? "");
        const text = hasQuoteRef(raw) ? stripAllQuoteRefs(raw) : raw;
        try {
            await navigator.clipboard.writeText(text);
        } catch {
            // 兜底：隐藏 textarea + execCommand
            const ta = document.createElement("textarea");
            ta.value = text;
            ta.style.position = "fixed";
            ta.style.opacity = "0";
            document.body.appendChild(ta);
            ta.select();
            try {
                document.execCommand("copy");
            } catch (e) {
                console.warn("复制失败:", e);
            }
            ta.remove();
        }
    }

    // ========== 双向懒加载状态 ==========
    let loading = $state(false);
    let loadError = $state<string | null>(null);
    let hasMoreOlder = $state(true); // 是否还有更早的历史消息可加载
    const PAGE_SIZE = 50;
    const LOAD_THRESHOLD = 200; // 滚动到距顶部多少 px 时触发加载
    // (ts, id) 游标边界，记录已加载消息的时间范围
    // ts 以秒为单位（与数据库格式一致）
    let oldestCursor = $state<{ ts: number; id: number } | null>(null);

    // 当 contactId 变化时，重新初始化；若携带跳转目标（搜索命中）则定位到该消息行
    $effect(() => {
        const cid = contactId;
        if (!cid) {
            msgs = [];
            loadedMsgIds = new Set();
            hasMoreOlder = true;
            oldestCursor = null;
            loading = false;
            loadError = null;
            return;
        }
        const jt = jumpTarget;
        if (jt && jt.peer === cid && jt.id !== lastJumpedId) {
            lastJumpedId = jt.id;
            void jumpToMessage(jt.id);
            return;
        }
        loadLatest(cid);
    });

    // ---------- 消息加载 ----------

    // invoke 超时保护：移动端 IPC 异常时防止 loading 永久卡住。
    // 超时取 30s（与后端 rr_file 分片超时同量级）：文件下载期间磁盘/SQLite
    // 繁忙时，消息加载仍有余量，避免误判"调用超时"中断正在进行的下载。
    function invokeWithTimeout<T>(cmd: string, args: Record<string, unknown>): Promise<T> {
        return new Promise<T>((resolve, reject) => {
            const timer = setTimeout(
                () => reject(new Error(`${cmd} 调用超时`)),
                30000,
            );
            invoke<T>(cmd, args)
                .then((v) => {
                    clearTimeout(timer);
                    resolve(v);
                })
                .catch((e) => {
                    clearTimeout(timer);
                    reject(e);
                });
        });
    }

    // 将后端消息行转换为前端 Msg 格式
    function parseBackend(m: {
        id: number;
        mldsa_pubkey_hex: string;
        content: string;
        is_outgoing: boolean;
        ts: number;
        pending: number;
    }): Msg {
        let type: Msg["type"] = "text";
        let file_hash_info: Msg["file_hash_info"] = undefined;

        // 尝试解析新版 file_hash 格式: "[文件] filename [hash:hex64]"
        const FILE_SHARE_PREFIX = "[文件] ";
        const FILE_SHARE_HASH_PREFIX = " [hash:";
        if (m.content.startsWith(FILE_SHARE_PREFIX)) {
            const hashStart = m.content.indexOf(FILE_SHARE_HASH_PREFIX);
            if (hashStart !== -1) {
                const filename = m.content
                    .substring(FILE_SHARE_PREFIX.length, hashStart)
                    .trim();
                const hashEnd = m.content.indexOf("]", hashStart + FILE_SHARE_HASH_PREFIX.length);
                if (hashEnd !== -1) {
                    const fileHash = m.content.substring(
                        hashStart + FILE_SHARE_HASH_PREFIX.length,
                        hashEnd,
                    );
                    // 校验 64-hex 与文件名长度：防止普通文本被伪装成文件卡片
                    // （与下方 JSON 分支同口径，点击才失败的"假文件"应保持 text）
                    if (
                        filename.length > 0 &&
                        filename.length <= 512 &&
                        /^[0-9a-fA-F]{64}$/.test(fileHash)
                    ) {
                        type = "file_hash";
                        file_hash_info = {
                            filename,
                            total_size: 0,
                            // 统一小写：后端进度事件 file_id 为 hex::encode（小写），
                            // 大小写不一致会导致 getFileProgress 查不到进度
                            file_hash: fileHash.toLowerCase(),
                        };
                    }
                }
            }
        }

        // 尝试解析旧版 JSON 格式（兼容历史消息）。
        // 仅当字段同时满足文件卡片结构时才升级为 file_hash 类型，
        // 避免任意 JSON 文本被恶意渲染为可点击的下载卡片。
        if (type === "text") {
            try {
                const parsed = JSON.parse(m.content);
                const isHex64 = typeof parsed.file_hash === "string" && /^[0-9a-fA-F]{64}$/.test(parsed.file_hash);
                if (
                    isHex64 &&
                    typeof parsed.filename === "string" &&
                    parsed.filename.length > 0 &&
                    parsed.filename.length <= 512
                ) {
                    type = "file_hash";
                    file_hash_info = {
                        filename: parsed.filename,
                        total_size: typeof parsed.total_size === "number" ? parsed.total_size : 0,
                        file_hash: parsed.file_hash.toLowerCase(),
                    };
                }
            } catch {
                // 不是 JSON，保持 text 类型
            }
        }

        return {
            id: `hist-${m.id}`,
            content:
                type === "file_hash"
                    ? `[文件] ${file_hash_info!.filename} (${formatFileSize(file_hash_info!.total_size)})`
                    : m.content,
            ts: m.ts * 1000,
            me: m.is_outgoing,
            type,
            file_hash_info,
            mldsa_pubkey_hex: m.mldsa_pubkey_hex,
            // 入站文件消息：发送方=对方公钥（下载请求由此发起）；出站=自己（本机定位）
            sender_mldsa_pubkey_hex: m.is_outgoing
                ? undefined
                : m.mldsa_pubkey_hex,
            pending: m.pending === 1 ? true : undefined,
        };
    }

    // 从后端提取数字 id（去掉 "hist-" 前缀）
    function extractNumericId(msgId: string): number {
        if (msgId.startsWith("hist-")) {
            return parseInt(msgId.slice(5), 10);
        }
        // 实时消息没有 "hist-" 前缀，返回 NaN
        return NaN;
    }

    // 加载序列号：串行化消息加载并丢弃过期会话的迟到结果。
    // 移动端超时后旧 invoke 仍可能在桥中排队，若不丢弃旧结果/不串行，
    // 每次打开会话都叠加新 invoke，越堆越多 → 持续"加载超时"。
    let loadSeq = 0;

    // 初始加载：获取最新消息
    async function loadLatest(peerId: string) {
        const cid = peerId;
        const seq = ++loadSeq;
        loading = true;
        for (let attempt = 0; ; attempt++) {
            try {
                const raw: {
                    id: number;
                    mldsa_pubkey_hex: string;
                    content: string;
                    is_outgoing: boolean;
                    ts: number;
                    pending: number;
                }[] = await invokeWithTimeout("load_messages", {
                    mldsaPubkeyHex: peerId,
                    limit: PAGE_SIZE,
                });
                // 已有更新的加载接管（换会话/重新打开）或用户已切换：丢弃本次结果
                if (seq !== loadSeq || cid !== contactId) return;
                // 反转顺序（数据库按时间倒序，前端需要正序）
                const loaded = raw.reverse().map(parseBackend);
                const ids = new Set(loaded.map((m) => m.id));
                msgs = loaded;
                loadedMsgIds = ids;
                // 如果返回数量 < limit，说明没有更早的消息了
                hasMoreOlder = raw.length >= PAGE_SIZE;
                // 更新游标
                if (loaded.length > 0) {
                    const id0 = extractNumericId(loaded[0].id);
                    oldestCursor = {
                        ts: Math.floor(loaded[0].ts / 1000),
                        id: isNaN(id0) ? 0 : id0,
                    };
                } else {
                    oldestCursor = null;
                    hasMoreOlder = false;
                }
                // 滚动到底部
                await tick();
                if (seq !== loadSeq || cid !== contactId) return;
                vlist?.scrollToIndex(msgs.length - 1, { smooth: false });
                loadError = null;
                return;
            } catch (e) {
                if (seq !== loadSeq || cid !== contactId) return;
                const msg = e instanceof Error ? e.message : String(e);
                // 调用超时常为失败传输/断连瞬间的事件突发挤占 IPC/WebView 主线程的
                // 瞬态现象（load_messages 后端纯 SQLite 无阻塞）：有界退避重试，
                // 风暴过去即恢复；多次重试仍失败才报错（不掩盖真实故障）。
                if (msg.includes("调用超时") && attempt < 3) {
                    const backoff = [800, 2000, 5000][attempt] ?? 800;
                    console.warn(
                        `load_messages 超时，${backoff}ms 后重试 (${attempt + 1}/3):`,
                        msg,
                    );
                    await new Promise((r) => setTimeout(r, backoff));
                    continue;
                }
                // 文件传输进行中时，消息加载超时/失败只是次要提示：
                // 不覆盖会话、不置 loadError，避免"加载消息失败"中断下载。
                if (Object.keys($transferTasks).length === 0) {
                    loadError = tt("load_history_failed", { values: { error: msg } });
                } else {
                    console.warn("传输期间加载消息失败（不中断下载）:", msg);
                }
                console.error("加载消息失败:", e);
                return;
            } finally {
                if (seq === loadSeq) loading = false;
            }
        }
    }

    // 定位到指定消息行（搜索命中跳转）：以目标为中心拉取窗口并滚动+高亮
    async function jumpToMessage(msgId: number) {
        const cid = contactId;
        if (!cid) return;
        loading = true;
        loadError = null;
        try {
            const raw: {
                id: number;
                mldsa_pubkey_hex: string;
                content: string;
                is_outgoing: boolean;
                ts: number;
                pending: number;
            }[] = await invokeWithTimeout("load_messages_around", {
                mldsaPubkeyHex: cid,
                msgId,
                radius: 25,
            });
            // 联系人已切换：本次定位属于旧会话，丢弃
            if (cid !== contactId) return;
            const loaded = raw.map(parseBackend);
            if (loaded.length === 0) {
                // 目标消息不存在（如引用标记被篡改或 id 已删除）：保留当前列表，不清空会话
                loadError = tt("message_not_found");
                return;
            }
            msgs = loaded;
            loadedMsgIds = new Set(loaded.map((m) => m.id));
            hasMoreOlder = raw.length >= 51; // 窗口拉满说明更早可能还有
            if (loaded.length > 0) {
                const id0 = extractNumericId(loaded[0].id);
                oldestCursor = {
                    ts: Math.floor(loaded[0].ts / 1000),
                    id: isNaN(id0) ? 0 : id0,
                };
            } else {
                oldestCursor = null;
                hasMoreOlder = false;
            }
            await tick();
            const targetKey = `hist-${msgId}`;
            const idx = msgs.findIndex((m) => m.id === targetKey);
            if (idx >= 0) {
                vlist?.scrollToIndex(idx, { smooth: false });
                highlightId = targetKey;
                setTimeout(() => (highlightId = null), 2500);
            }
        } catch (e) {
            const msg = e instanceof Error ? e.message : String(e);
            loadError = tt("locate_message_failed", { values: { error: msg } });
        } finally {
            loading = false;
        }
    }

    // 加载更早的消息（上向翻页 —— 用户向上滚动触顶时调用）
    async function loadOlder() {
        if (
            loading ||
            !hasMoreOlder ||
            msgs.length === 0 ||
            !contactId ||
            !oldestCursor
        )
            return;
        const cid = contactId;
        const cursor = oldestCursor;
        loading = true;
        // 记录当前滚动偏移，用于 prepend 后恢复
        const scrollPos = vlist?.getScrollOffset() ?? 0;
        try {
            const raw: {
                id: number;
                mldsa_pubkey_hex: string;
                content: string;
                is_outgoing: boolean;
                ts: number;
                pending: number;
            }[] = await invokeWithTimeout("load_messages", {
                mldsaPubkeyHex: cid,
                before: cursor.ts,
                beforeId: cursor.id,
                limit: PAGE_SIZE,
            });
            // 联系人已切换：本次翻页属于旧会话，丢弃
            if (cid !== contactId) return;
            if (raw.length < PAGE_SIZE) hasMoreOlder = false;
            if (raw.length === 0) return;
            // 解析、反转（DESC → ASC）、去重
            const parsed = raw.reverse().map(parseBackend);
            // 已加载 id 集合可能因滑动淘汰而不含旧消息 id（msgs 仍保留）：
            // 双重检查 msgs 中已存在的 id，避免重复追加同一条消息
            const existing = new Set(msgs.map((m) => m.id));
            const newOnes = parsed.filter((m) => !loadedMsgIds.has(m.id) && !existing.has(m.id));
            if (newOnes.length === 0) {
                hasMoreOlder = false; // 剩余的都是重复内容，说明已到尽头
                return;
            }
            // 更新游标（新加载消息中最旧的那条）
            const firstNew = newOnes[0];
            const firstNewId = extractNumericId(firstNew.id);
            oldestCursor = {
                ts: Math.floor(firstNew.ts / 1000),
                id: isNaN(firstNewId) ? 0 : firstNewId,
            };
            // Prepending
            const count = newOnes.length;
            msgs = [...newOnes, ...msgs];
            newOnes.forEach((m) => dedupAdd(m.id));
            await tick();
            // 滚动位置补偿：prepend 的内容将旧内容推下去了，
            // 需要向下滚动补偿新内容的总高度
            vlist?.scrollTo(scrollPos + count * 80);
        } catch (e) {
            console.error("加载更早消息失败:", e);
        } finally {
            loading = false;
        }
    }

    // ---------- Scroll 事件处理 ----------

    function handleScroll(offset: number) {
        if (!vlist || loading) return;
        // 这里的 offset 是 VList 传入的滚动位置，使用它判断是否触底/触顶。
        if (offset < LOAD_THRESHOLD && hasMoreOlder) {
            loadOlder();
        }
    }

    // ---------- 以下函数与改造前完全兼容 ----------

    export function add(
        text: string,
        me = false,
        type: "text" | "file_hash" | "file_stream" = "text",
        file_hash_info?: Msg["file_hash_info"],
        sender_mldsa_pubkey_hex?: string,
        mldsa_pubkey_hex?: string,
        pending?: boolean,
        message_hash?: string,
    ) {
        // 如果指定了 mldsa_pubkey_hex 且与当前选中的联系人不同，则忽略此消息
        if (mldsa_pubkey_hex && contactId && mldsa_pubkey_hex !== contactId) {
            return text;
        }
        if (!text.trim() && type === "text") return text;
        const content = finalizeQuoteRef(text.trim());
        const newMsg: Msg = {
            id: `${Date.now()}-${Math.random().toString(36).slice(2, 7)}`,
            content,
            ts: Date.now(),
            me,
            type,
            file_hash_info,
            sender_mldsa_pubkey_hex,
            mldsa_pubkey_hex: mldsa_pubkey_hex || contactId || "",
            pending,
            message_hash,
        };
        // 去重：如果消息 ID 已存在则跳过
        if (loadedMsgIds.has(newMsg.id)) return content;
        msgs.push(newMsg);
        dedupAdd(newMsg.id);
        tick().then(() =>
            vlist?.scrollToIndex(msgs.length - 1, { smooth: true }),
        );
        return content;
    }

    // 更新消息发送状态（从 pending 变为 sent）
    export function markSent(messageHash: string) {
        // 优先按 message_hash 精确匹配对应的 pending 消息
        for (let i = msgs.length - 1; i >= 0; i--) {
            const msg = msgs[i];
            if (msg.me && msg.message_hash === messageHash) {
                msg.pending = false;
                return;
            }
        }
        // 精确匹配失败（如历史 pending 未存 hash）：仅当本地只有一条 pending
        // 时才降级标记（无歧义）；多条 pending 时保持等待精确回执，
        // 避免把错误的待发消息标记为已送达（"看似已提交实际未发送"的反向误标）。
        const pendings: number[] = [];
        for (let i = 0; i < msgs.length; i++) {
            if (msgs[i].me && msgs[i].pending === true) pendings.push(i);
        }
        if (pendings.length === 1) {
            msgs[pendings[0]].pending = false;
        }
    }

    // 更新最近一条 pending 消息的 message_hash 字段
    // 由后端发送 message-sent 事件时调用，用于后续送达回执精确匹配
    export function updateMessageHash(messageHash: string) {
        // 从后往前找最近一条没有 message_hash 的 pending 消息
        for (let i = msgs.length - 1; i >= 0; i--) {
            const msg = msgs[i];
            if (msg.me && msg.pending === true && !msg.message_hash) {
                msg.message_hash = messageHash;
                return;
            }
        }
    }

    export async function del(id: string) {
        // 如果是历史消息（id 格式为 "hist-{数字}"），调用后端删除
        if (id.startsWith("hist-")) {
            const msgId = parseInt(id.slice(5), 10);
            if (!isNaN(msgId)) {
                try {
                    await invoke("delete_message", { messageId: msgId });
                } catch (e) {
                    console.error("删除消息失败:", e);
                    return; // 后端删除失败，不更新 UI
                }
            }
        }
        // 前端 UI 移除
        msgs = msgs.filter((m) => m.id !== id);
        loadedMsgIds.delete(id);
    }

    // 计算进度百分比（统一由全局 transfers.calcProgress 提供）

// 点击 FileHash 消息：入站文件下载（后端"另存为"）；自己发送的文件本机定位打开
    async function handleFileHashClick(msg: Msg) {
        if (!msg.file_hash_info) return;
        const info = msg.file_hash_info;

        // 自己发送的文件：本机定位（文件仍在本地；换设备重拉为后续功能）
        if (msg.me) {
            try {
                await invoke("reveal_own_sent_file", {
                    fileHashHex: info.file_hash,
                });
            } catch (e) {
                console.error("定位本地文件失败:", e);
            }
            return;
        }

        if (!msg.sender_mldsa_pubkey_hex) return;

        // 检查是否已经在下载中（按 file_id 精确匹配）
        if (info.file_hash && $transferTasks[info.file_hash]?.status === "downloading") {
            return;
        }
        if (Object.values($transferTasks).some(
            (p) => p.filename === info.filename && p.status === "downloading",
        )) {
            return;
        }

        try {
            await invoke("request_file_download", {
                senderMldsaPubkeyHex: msg.sender_mldsa_pubkey_hex,
                fileHashHex: info.file_hash,
                filename: info.filename,
            });
        } catch (e) {
            console.error("请求文件下载失败:", e);
        }
    }

    // 获取文件传输进度（如果有）
    // tasks 由模板以 $transferTasks 传入：Svelte 在模板中对 $store 建立响应式依赖，
    // store 变化时卡片重渲染；函数内部不可用 $store（编译为 get()，非跟踪读取）。
    function getFileProgress(
        msg: Msg,
        tasks: Record<string, FileTransferProgress>,
    ): FileTransferProgress | undefined {
        if (msg.type === "file_stream") {
            // 入站推送传输：按文件名反查
            return Object.values(tasks).find(
                (p) => p.filename === msg.content && p.status !== "completed",
            );
        }
        if (msg.file_hash_info) {
            // 统一小写再查：后端 file_id 为 hex::encode（小写）
            const byHash = tasks[msg.file_hash_info.file_hash.toLowerCase()];
            if (byHash) return byHash;
            return Object.values(tasks).find(
                (p) =>
                    p.filename === msg.file_hash_info!.filename &&
                    (p.status === "downloading" || p.status === "error"),
            );
        }
        return undefined;
    }

    // 中断传输：后端发 Error 终态事件驱动卡片显示失败（可重试/删除），故不在本地清除
    async function cancelTransfer(p: FileTransferProgress) {
        if (!p.file_id) return;
        try {
            await invoke("cancel_file_transfer", { fileIdHex: p.file_id });
        } catch (e) {
            console.error("取消传输失败:", e);
        }
    }

    // 删除任务：后端清理任务状态/临时文件，本地移除全局进度条目（卡片回到普通文件消息）
    async function deleteTransfer(p: FileTransferProgress) {
        if (!p.file_id) return;
        try {
            await invoke("cancel_file_transfer", { fileIdHex: p.file_id });
            removeTransfer(p.file_id);
        } catch (e) {
            console.error("删除传输任务失败:", e);
        }
    }
</script>

<VList
    bind:this={vlist}
    data={msgs}
    getKey={(m: Msg) => m.id}
    class="list"
    onscroll={handleScroll}
>
    {#snippet children(m: Msg)}
        <div
                class="msg"
                class:me={m.me}
                class:highlight={m.id === highlightId}
            >
                <!-- svelte-ignore a11y_no_static_element_interactions -->
                <div
                    class="bubble"
                    class:file-hash={m.type === "file_hash"}
                    style="font-size: {fontSize}"
                    oncontextmenu={(e) => {
                        e.preventDefault();
                        openMsgMenuAt(e.clientX, e.clientY, m);
                    }}
                    onpointerdown={(e) => handlePointerDown(e, m)}
                    onpointerup={clearLongPress}
                    onpointermove={clearLongPress}
                    onpointercancel={clearLongPress}
                >
                {#if m.type === "file_hash" && m.file_hash_info}
                    <!-- FileHash 消息：可点击下载 -->
                    <div
                        class="file-hash-content"
                        onclick={() => handleFileHashClick(m)}
                        onkeydown={(e) =>
                            e.key === "Enter" && handleFileHashClick(m)}
                        role="button"
                        tabindex="0"
                        aria-label={$_("click_to_download_file")}
                    >
                        <span class="file-badge">{$_("file")}</span>
                        <div class="file-icon">
                            <svg
                                width="24"
                                height="24"
                                viewBox="0 0 24 24"
                                fill="none"
                                stroke="currentColor"
                                stroke-width="2"
                            >
                                <path
                                    d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"
                                />
                                <polyline points="14 2 14 8 20 8" />
                                <line x1="12" y1="18" x2="12" y2="12" />
                                <line x1="9" y1="15" x2="15" y2="15" />
                            </svg>
                        </div>
                        <div class="file-info">
                            <span class="file-name"
                                >{m.file_hash_info.filename}</span
                            >
                            <span class="file-size"
                                >{formatFileSize(
                                    m.file_hash_info.total_size,
                                )}</span
                            >
                            <span class="file-hash-label">{$_("click_to_download_file")}</span>

                            {#if getFileProgress(m, $transferTasks)}
                                {@const p = getFileProgress(m, $transferTasks)!}
                                <div class="download-progress">
                                    <div class="progress-bar">
                                        <div
                                            class="progress-fill"
                                            class:completed={p.status ===
                                                "completed"}
                                            style="width: {calcProgress(p)}%"
                                        ></div>
                                    </div>
                                    <span class="progress-text">
                                        {#if p.status === "downloading"}
                                            {$_("downloading_progress", { values: { p: calcProgress(p) } })}
                                            <button
                                                class="progress-cancel"
                                                onclick={() => cancelTransfer(p)}
                                                aria-label={$_("action_cancel")}
                                                >{$_("action_cancel")}</button
                                            >
                                        {:else if p.status === "completed"}
                                            {$_("download_complete")}
                                        {:else if p.status === "error" ||
                                            p.status === "failed"}
                                            {$_("download_failed")}
                                            <button
                                                class="progress-cancel"
                                                onclick={() =>
                                                    handleFileHashClick(m)}
                                                aria-label={$_("action_retry")}
                                                >{$_("action_retry")}</button
                                            >
                                            <button
                                                class="progress-cancel"
                                                onclick={() => deleteTransfer(p)}
                                                aria-label={$_("action_delete")}
                                                >{$_("action_delete")}</button
                                            >
                                        {/if}
                                    </span>
                                </div>
                            {/if}
                        </div>
                    </div>
                {:else if m.type === "file_stream"}
                    <!-- file_stream 类型消息（接收文件传输中） -->
                    <div class="file-stream-content">
                        <span class="file-badge">{$_("file")}</span>
                        <div class="file-icon">
                            <svg
                                width="24"
                                height="24"
                                viewBox="0 0 24 24"
                                fill="none"
                                stroke="currentColor"
                                stroke-width="2"
                            >
                                <path
                                    d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"
                                />
                                <polyline points="14 2 14 8 20 8" />
                            </svg>
                        </div>
                        <span>{m.content}</span>
                        {#if getFileProgress(m, $transferTasks)}
                            {@const fp = getFileProgress(m, $transferTasks)!}
                            <div class="download-progress">
                                <div class="progress-bar">
                                    <div
                                        class="progress-fill"
                                        style="width: {calcProgress(fp)}%"
                                    ></div>
                                </div>
                                <span class="progress-text">
                                    {#if fp.status === "downloading"}
                                        {$_("receiving_progress", { values: { p: calcProgress(fp) } })}
                                        <button
                                            class="progress-cancel"
                                            onclick={() => cancelTransfer(fp)}
                                            aria-label={$_("action_cancel")}
                                            >{$_("action_cancel")}</button
                                        >
                                    {:else if fp.status === "error" ||
                                        fp.status === "failed"}
                                        {$_("receive_failed")}
                                        <button
                                            class="progress-cancel"
                                            onclick={() => deleteTransfer(fp)}
                                            aria-label={$_("action_delete")}
                                            >{$_("action_delete")}</button
                                        >
                                    {/if}
                                </span>
                            </div>
                        {/if}
                    </div>
                {:else if m.type === "text"}
                    {@const isQuote = hasQuoteRef(m.content)}
                    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
                    <div
                        class="msg-text"
                        class:has-quote-ref={isQuote}
                        role={isQuote ? "button" : undefined}
                        tabindex={isQuote ? 0 : undefined}
                        onclick={isQuote
                            ? (e: MouseEvent) => {
                                  // 落在真实链接上时不触发引用跳转（链接由全局处理器打开，
                                  // 避免一次点击同时"开链接 + 跳列表"的误导性行为）
                                  if ((e.target as HTMLElement)?.closest?.("a"))
                                      return;
                                  jumpToQuote(m);
                              }
                            : undefined}
                        onkeydown={isQuote
                            ? (e: KeyboardEvent) => {
                                  if (e.key === "Enter" || e.key === " ")
                                      jumpToQuote(m);
                              }
                            : undefined}
                    >
                        {@html renderMarkdown(isQuote ? stripAllQuoteRefs(m.content) : m.content)}
                    </div>
                {/if}
            </div>
            <div class="msg-meta">
                <time
                    >{new Date(m.ts).toLocaleTimeString([], {
                        hour: "2-digit",
                        minute: "2-digit",
                    })}</time
                >
                {#if m.pending === true}
                    <span class="pending-indicator">{$_("sending_indicator")}</span>
                {:else if m.pending === false}
                    <span class="sent-indicator">✓</span>
                {/if}
            </div>
            <button class="x" onclick={() => del(m.id)} aria-label={$_("delete_message")}
                >×</button
            >
        </div>
    {/snippet}
</VList>

{#if msgMenu}
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <div
        class="msg-menu-overlay"
        role="presentation"
        aria-hidden="true"
        onclick={() => (msgMenu = null)}
    ></div>
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <div
        class="msg-menu"
        style="left: {msgMenu.x}px; top: {msgMenu.y}px"
        role="menu"
        tabindex="-1"
        onclick={(e) => e.stopPropagation()}
    >
        <button type="button" class="msg-menu-item" onclick={() => copyMessage(msgMenu!.msg)}>
            {$_("copy")}
        </button>
        <button type="button" class="msg-menu-item" onclick={() => quoteMessage(msgMenu!.msg)}>
            {$_("quote")}
        </button>
    </div>
{/if}

{#if loading}
    <div class="loading-hint">{$_("loading")}...</div>
{/if}
{#if loadError}
    <div class="load-error-hint">{loadError}</div>
{/if}

<style>
    :global(.virtua-scroll-view) {
        padding: 16px;
        padding-top: 48px;
        background: transparent;
    }
    @container (max-width: 480px) {
        :global(.virtua-scroll-view) {
            padding: 8px;
            padding-top: 48px;
        }
        .msg {
            padding-right: 8px;
        }
        .msg.me {
            padding-left: 8px;
        }
        .bubble {
            max-width: 90%;
        }
        .msg-meta time {
            font-size: 10px;
        }
    }
    :global(.list) {
        height: 100%;
        display: flex;
        flex-direction: column;
        container-type: inline-size;
    }
    .msg {
        display: flex;
        flex-direction: column;
        align-items: flex-start;
        margin-bottom: var(--spacing-xs, 8px);
        position: relative;
        padding-right: 36px;
        gap: 2px;
    }
    .msg.me {
        align-items: flex-end;
        padding-right: 0;
        padding-left: 36px;
    }
    /* 搜索跳转高亮：气泡描边 */
    .msg.highlight .bubble {
        outline: 2px solid var(--color-primary);
        outline-offset: 2px;
    }

    /* 消息操作菜单（长按/右键） */
    .msg-menu-overlay {
        position: fixed;
        inset: 0;
        z-index: 1200;
        background: transparent;
    }
    .msg-menu {
        position: fixed;
        z-index: 1201;
        min-width: 120px;
        padding: 4px;
        border-radius: 10px;
        background: var(--bg-secondary, #1a1a2e);
        border: 1px solid var(--border-color, #2a2a2a);
        box-shadow: 0 4px 16px rgba(0, 0, 0, 0.35);
    }
    .msg-menu-item {
        display: block;
        width: 100%;
        text-align: left;
        padding: 8px 12px;
        border: none;
        background: transparent;
        color: var(--text-primary, #fafafa);
        font-size: 13px;
        cursor: pointer;
        border-radius: 6px;
    }
    .msg-menu-item:hover {
        background: var(--color-hover);
        color: var(--color-primary);
    }
    .bubble {
        max-width: var(--bubble-max, 70%);
        padding: var(--bubble-padding, 8px 12px);
        border-radius: var(--bubble-radius, 12px);
        background: var(--bg-secondary, #2a2a2a);
        word-break: break-word;
        white-space: pre-wrap;
    }
    @container (max-width: 400px) {
        .bubble {
            max-width: 85%;
        }
        .msg {
            margin-bottom: 4px;
        }
    }
    .msg.me .bubble {
        background: var(--color-primary);
    }

    /* FileHash 气泡样式：主色浅底卡片，与 md 文本气泡绝对区分 */
    .bubble.file-hash {
        background: var(--color-primary-soft);
        border: 1px solid var(--color-primary);
        cursor: pointer;
        padding: 8px;
    }
    .msg.me .bubble.file-hash {
        background: var(--color-primary-strong);
        border-color: var(--color-primary);
    }

    .file-hash-content {
        display: flex;
        flex-direction: column;
        gap: 8px;
        user-select: none;
    }

    /* 文件消息徽标：与 md 文本消息做绝对区分的小字"文件"标签 */
    .file-badge {
        align-self: flex-start;
        flex-shrink: 0;
        font-size: 10px;
        line-height: 1;
        font-weight: 600;
        letter-spacing: 0.05em;
        padding: 2px 7px;
        border-radius: 999px;
        border: 1px solid var(--color-primary);
        color: var(--color-primary);
        background: var(--bg-primary, #0f0f0f);
        user-select: none;
    }

    .file-hash-content:hover .file-name {
        color: var(--color-primary);
    }

    .file-stream-content {
        display: flex;
        flex-wrap: wrap;
        gap: 8px 12px;
        align-items: center;
        background: var(--color-primary-soft);
        border: 1px solid var(--color-primary);
        border-radius: 10px;
        padding: 8px 10px;
    }
    .file-stream-content .file-badge {
        flex-basis: 100%;
        margin-bottom: -4px;
    }
    .file-stream-content > span:not(.file-badge):not(.progress-text) {
        flex: 1;
        min-width: 0;
        word-break: break-all;
    }

    .file-icon {
        flex-shrink: 0;
        opacity: 0.8;
    }

    .file-info {
        display: flex;
        flex-direction: column;
        gap: 2px;
        min-width: 0;
    }

    .file-name {
        font-weight: 600;
        font-size: 14px;
        word-break: break-all;
        transition: color 0.2s;
    }

    .file-size {
        font-size: 12px;
        opacity: 0.7;
    }

    .file-hash-label {
        font-size: 11px;
        opacity: 0.6;
        font-style: italic;
    }

    /* 下载进度条 */
    .download-progress {
        display: flex;
        flex-direction: column;
        gap: 4px;
        margin-top: 4px;
    }

    .progress-bar {
        width: 100%;
        height: 6px;
        background: rgba(255, 255, 255, 0.15);
        border-radius: 3px;
        overflow: hidden;
    }

    .progress-fill {
        height: 100%;
        background: var(--color-primary);
        border-radius: 3px;
        transition: width 0.3s ease;
    }

    .progress-fill.completed {
        background: var(--color-success);
    }

    .progress-text {
        font-size: 11px;
        opacity: 0.7;
        display: flex;
        align-items: center;
        gap: 6px;
    }

    .progress-cancel {
        font-size: 10px;
        line-height: 1;
        padding: 2px 6px;
        border-radius: 4px;
        border: 1px solid rgba(255, 255, 255, 0.3);
        background: transparent;
        color: inherit;
        cursor: pointer;
        opacity: 0.9;
    }

    .progress-cancel:hover {
        background: rgba(255, 255, 255, 0.15);
    }

    /* Markdown 渲染内容 */
    .msg-text {
        line-height: 1.5;
        word-break: break-word;
        font-size: inherit;
    }
    /* 带引用标记的消息气泡：可点击跳转到原消息 */
    .msg-text.has-quote-ref {
        cursor: pointer;
    }
    .msg-text.has-quote-ref:hover {
        opacity: 0.85;
    }
    .msg-text :global(p) {
        margin: 0 0 6px;
    }
    .msg-text :global(p:last-child) {
        margin-bottom: 0;
    }
    .msg-text :global(a) {
        color: inherit;
    }
    /* 防误导链接：真实目标弱化小字（markdown.ts 在文本≠href 时追加） */
    .msg-text :global(.md-link-url) {
        font-size: 11px;
        opacity: 0.6;
        word-break: break-all;
        margin-left: 2px;
    }
    .msg-text :global(code) {
        background: rgba(128, 128, 128, 0.25);
        border-radius: 4px;
        padding: 0 4px;
        font-size: 0.9em;
    }
    .msg-text :global(pre) {
        overflow-x: auto;
        margin: 6px 0;
    }
    .msg-text :global(blockquote) {
        border-left: 3px solid rgba(128, 128, 128, 0.5);
        margin: 4px 0;
        padding-left: 8px;
        opacity: 0.85;
    }
    .msg-meta {
        display: flex;
        align-items: center;
        gap: 4px;
        margin-top: 4px;
    }
    .msg-meta time {
        font-size: 11px;
        opacity: 0.6;
    }
    .pending-indicator {
        font-size: 11px;
        opacity: 0.7;
        animation: pulse 1.5s ease-in-out infinite;
    }
    .sent-indicator {
        font-size: 11px;
        color: var(--color-success);
        opacity: 0.8;
    }
    @keyframes pulse {
        0%,
        50% {
            opacity: 0.7;
        }
        50% {
            opacity: 0.3;
        }
    }

    .x {
        width: 20px;
        height: 20px;
        border-radius: 50%;
        border: none;
        background: transparent;
        color: #666;
        cursor: pointer;
        opacity: 0;
        position: absolute;
        right: 8px;
        top: 50%;
        transform: translateY(-50%);
    }
    .msg:hover .x {
        opacity: 1;
    }
    .x:hover {
        background: var(--color-danger-soft);
        color: var(--color-danger);
    }
    .msg.me .x {
        right: auto;
        left: 8px;
    }

    .loading-hint {
        text-align: center;
        font-size: 12px;
        color: #888;
        padding: 4px;
        position: absolute;
        top: 0;
        left: 50%;
        transform: translateX(-50%);
        pointer-events: none;
    }

    .load-error-hint {
        text-align: center;
        font-size: 12px;
        color: #e74c3c;
        padding: 4px;
        position: absolute;
        top: 24px;
        left: 50%;
        transform: translateX(-50%);
        pointer-events: none;
    }
</style>
