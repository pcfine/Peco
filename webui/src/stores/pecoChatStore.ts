import { create } from "zustand";
import {
  getPecoSession,
  clearPecoSession,
  cancelPecoStream,
  pecoStreamUrl,
  pecoAttachUrl,
  queryPecoStream,
} from "@/api/peco";
import { parseSSELines, toChatSseEvent } from "@/api/stream";
import { useAuthStore } from "@/stores/authStore";
import {
  snapshotToMessages,
  reduceStreamEvent,
  isStreamTerminalEvent,
} from "@/components/chat/ChatView";
import type { ChatMessage } from "@/components/chat/ChatView";
import type {
  ChatSseEvent,
  SessionSnapshotResponse,
  UsageData,
} from "@/types/chat";

// ── 常驻 SSE 连接 ────────────────────────────────────────────────────────
//
// 一个窗口任意时刻至多一条连接。服务端把同一个 run 的事件 broadcast 给
// **每个**订阅者，多开一条就会让同一批 delta 被重复消费（界面文字成对重复）。
// 后续消息走 POST /stream/query 投递，复用这条连接渲染。

interface StreamConn {
  controller: AbortController;
  /** 代际：事件应用与清理都要校验，防旧连接写坏新连接的状态。 */
  gen: number;
  /** 服务端已宣告 run 收尾（done）——下次发送不再复用，走引导路径。 */
  terminal: boolean;
}

let conn: StreamConn | null = null;
let nextGen = 0;
/** 一次性自愈：意外断流最多触发一次重附着，用户发消息后重新武装。 */
let selfHealUsed = false;

/** 关闭并作废当前连接（在途 reader 的一切写入随之失效）。 */
function closeStream(): void {
  if (!conn) return;
  conn.controller.abort();
  conn = null;
  nextGen += 1;
}

/**
 * 建立连接；已有活连接时为幂等 no-op（返回 false）。
 *
 * `conn` 必须在任何 await 之前同步赋值 —— 否则并发的两个调用方
 * （发送与获焦刷新）会同时看到「无连接」而各开一条。
 */
function openStream(
  url: string,
  token: string,
  set: (partial: Partial<PecoChatState>) => void,
  get: () => PecoChatState,
  opts?: { replace?: boolean },
): boolean {
  if (opts?.replace) closeStream();
  if (conn && !conn.terminal) return false;
  if (conn) closeStream();

  const controller = new AbortController();
  const gen = ++nextGen;
  conn = { controller, gen, terminal: false };
  void readStream(url, token, set, get, controller, gen);
  return true;
}

/**
 * 消费一条 SSE 流直到结束。
 *
 * 连接由 store 持有并跨消息复用：服务端在正常轮次结束时不发 `done`
 * （`done` 只随 run 收尾发出），所以读到流结束即意味着连接真的断了。
 */
async function readStream(
  url: string,
  token: string,
  set: (partial: Partial<PecoChatState>) => void,
  get: () => PecoChatState,
  controller: AbortController,
  gen: number,
): Promise<void> {
  let sawDone = false;

  try {
    const response = await fetch(url, {
      headers: { Authorization: `Bearer ${token}` },
      signal: controller.signal,
    });

    if (!response.ok) {
      // Expired/invalid token — log out so ProtectedRoute redirects to
      // /login, rather than showing the raw JSON error in the chat box.
      if (response.status === 401) {
        useAuthStore.getState().logout();
        throw new Error("登录已过期，请重新登录");
      }

      // Surface a human-readable message from the API error body
      // ({ error, details }) instead of a raw JSON blob.
      let message = `HTTP ${response.status}`;
      try {
        const body = (await response.json()) as {
          details?: string;
          error?: string;
        };
        message = body.details || body.error || message;
      } catch {
        // Non-JSON body — fall back to the status code.
      }
      throw new Error(message);
    }

    const reader = response.body!.getReader();
    const decoder = new TextDecoder();
    let buffer = "";
    let pendingEvent = "";

    while (true) {
      const { done, value } = await reader.read();
      if (done) break;

      const chunk = decoder.decode(value, { stream: true });
      const {
        events,
        remaining,
        pendingEvent: next,
      } = parseSSELines(chunk, buffer, pendingEvent);
      buffer = remaining;
      pendingEvent = next;

      // 代际校验：被替换/关闭的旧连接不得再写状态。有意替换时新旧
      // reader 会短暂并存，只在清理处校验挡不住这段重叠。
      if (conn?.gen !== gen) return;

      for (const parsed of events) {
        const event = toChatSseEvent(parsed);
        if (!event) continue;
        if (event.event === "done") sawDone = true;
        applyStreamEvent(event, set, get);
      }
    }
  } catch (err: unknown) {
    if (err instanceof Error && err.name === "AbortError") return;
    // Surface network / server errors so the UI can display a toast.
    const message = err instanceof Error ? err.message : "连接中断，请重试";
    set({ error: message });
  } finally {
    // 已被替换或关闭：不碰新连接的状态
    if (conn?.gen !== gen) return;
    conn = null;
    set({ isStreaming: false });

    // 未收到 done 的收尾属意外断流（服务端重启 / 网络抖动）→ 一次性重附着，
    // 由快照决定是接上在途轮次还是只重建历史。
    if (!sawDone && !controller.signal.aborted && !selfHealUsed) {
      selfHealUsed = true;
      void refreshSession(set, get).catch(() => {});
    }
  }
}

/** 每页拉取的 turn 数（尾部页与「加载更早」页一致）。 */
const PAGE_TURNS = 3;

/** pinned 摘要归档分隔线（恒在列表最前，turnIndex 0 与 turn 0 撞号但不重复）。 */
function pinnedNotice(summary: string): ChatMessage {
  return {
    id: "pinned-summary",
    role: "assistant",
    content: "更早的对话已归档为摘要，仍在模型上下文中",
    turnIndex: 0,
    isNotice: true,
    summary,
  };
}

/** 识别 pinned 摘要分隔线（与 context_compacted / 中断横幅区分）。 */
function isPinnedNotice(m: ChatMessage): boolean {
  return m.id === "pinned-summary";
}

/** 由快照拼装「尾部窗口」消息列表（pinned 摘要 + turns）。 */
function buildTailWindow(snap: SessionSnapshotResponse): ChatMessage[] {
  const restored = snapshotToMessages(snap.turns);
  return snap.pinned_summary
    ? [pinnedNotice(snap.pinned_summary), ...restored]
    : restored;
}

/**
 * 拉取尾部页（最新 PAGE_TURNS 轮）并按 turn 段合并进消息列表。
 *
 * 新不变量（分页后）：
 *  - 尾部窗口由快照权威替换，可自愈本窗口与其他窗口/服务端之间的漂移；
 *  - 窗口之前的旧页仅在 revision（compaction_count）不变的条件下可信，
 *    变了即丢弃 —— compaction 会让位置号整体前移，旧页游标全部失效。
 *
 * load()（首载）与 refresh()（窗口获焦追平）共用。快照不含进行中轮次，
 * 调用方须保证本窗口不在流式中（isStreaming 空闲）。
 */
async function refreshSession(
  set: (partial: Partial<PecoChatState>) => void,
  get: () => PecoChatState,
): Promise<void> {
  if (get().isStreaming) return;

  const snap = await getPecoSession({ turns: PAGE_TURNS });

  // 快照在途期间用户可能刚发出消息（sendMessage 会置 isStreaming）：
  // 此时整表替换会抹掉刚追加的用户消息与占位，必须放弃这次快照。
  if (get().isStreaming) return;

  const rev = snap.context_metrics?.compaction_count ?? 0;
  const revisionChanged = rev !== get().historyRevision;
  const tailStart = snap.turns.length > 0 ? snap.turns[0].turn_index : null;
  const tailWindow = buildTailWindow(snap);

  let messages: ChatMessage[];
  let oldestLoadedTurn: number | null;
  let hasMore: boolean;

  // 本地乐观尾部：列表末尾连续的「无 id」消息（乐观 user + 空占位 + 运行中
  // 横幅）。它们代表服务端尚未 committed 的本轮 —— 快照只含 committed_turns，
  // 任务进行中时本轮不在快照里；若照旧无条件丢弃，用户刚发的 query 会瞬间
  // 从列表消失，直到该轮落盘后才回来。
  const live = get().messages;
  let localTailStart = live.length;
  while (localTailStart > 0 && live[localTailStart - 1].id === undefined) {
    localTailStart -= 1;
  }
  // 尾部可能跨多轮：上一轮 turn_complete 后未追平就又发了一条。轮次按序落盘，
  // 更早的本地轮快照里必已有 —— 只保留最新一段，否则它会与尾部窗口里的同一条
  // 成对重复渲染；去重判据也必须取这一段的用户消息，拿整段的第一条比对会既漏又错。
  const localTailAll = live.slice(localTailStart);
  let localTail = localTailAll;
  for (let i = localTailAll.length - 1; i > 0; i -= 1) {
    if (localTailAll[i].role === "user") {
      localTail = localTailAll.slice(i);
      break;
    }
  }
  // 只看「最后一条已 committed 轮」的用户消息：本轮若已落盘，只可能是它（轮次按序
  // 追加、落在窗口末尾）。**不能**用整个窗口 —— 用户重复发同一句话（如「继续」）时，
  // 在途轮会被历史里的同文本误判为已落盘而丢弃，刚发的 query 再次消失。
  const lastCommittedUser = [...tailWindow]
    .reverse()
    .find((m) => m.role === "user")?.content;
  const localUser = localTail.find((m) => m.role === "user");
  // 本地 query 已出现在快照里 = 本轮已 committed → 以快照为准，丢弃本地副本
  // （否则与尾部窗口重复渲染）。尚未出现 = 在途 → 必须保留。
  const keepLocal =
    localTail.length > 0 &&
    (localUser !== undefined
      ? lastCommittedUser !== localUser.content
      : snap.turn_in_flight === true);

  if (revisionChanged || get().oldestLoadedTurn === null || tailStart === null) {
    // 首载 / 历史版本变化（compaction）/ 快照无轮：丢弃旧页，只留尾部窗口。
    messages = tailWindow;
    oldestLoadedTurn = tailStart;
    hasMore = snap.has_more ?? false;
  } else {
    // 版本未变：turn 段级替换尾部窗口，保留更早的已加载轮。
    //
    // 只保留「快照来源」的旧页：快照消息恒有 id（snapshotToMessages 赋
    // `turn-<位置号>-<轮内序号>`），而流式残留（刚发的那轮、占位气泡、banner）
    // 恒无 id 且 turnIndex 恒 0 —— 不按 id 过滤，这些残留就满足 `0 < tailStart`
    // 被当成旧页保留，与尾部窗口里的同一条轮重复渲染（发完消息切走切回即见）。
    //
    // pinned 分隔线恒由尾部窗口权威携带（快照无摘要时即不存在），且必须恒在最前：
    // 先从旧页剥除（其 turnIndex 恒为 0，会被 < tailStart 选中而与尾部那条重复），
    // 再整体前置 —— 否则更早的已加载轮会插到它前面，分隔线错位到列表中间。
    const prevOldest = get().oldestLoadedTurn;
    const head = get().messages.filter(
      (m) => m.id !== undefined && !isPinnedNotice(m) && m.turnIndex < tailStart,
    );
    const pinned =
      tailWindow.length > 0 && isPinnedNotice(tailWindow[0])
        ? [tailWindow[0]]
        : [];
    messages = [...pinned, ...head, ...tailWindow.slice(pinned.length)];
    oldestLoadedTurn = prevOldest;
    // 尾页 has_more 只说明「尾窗口之前还有轮」（= total > PAGE_TURNS），恒真；
    // 已加载到 turn 0 时前面已无可加载轮，必须据 oldestLoadedTurn 判定，
    // 否则翻到底后「加载更早」按钮假重现（点击拉空页后才自愈为 false）。
    hasMore = (prevOldest ?? 0) > 0;
  }

  if (keepLocal) {
    messages = [...messages, ...localTail];
  }

  // 整页刷新（冷启动）恢复在途轮的 query：此时前端内存全清、localTail 必空，
  // 而快照只含 committed_turns —— 不补这一条，用户刚发的 query 会一直消失到
  // 本轮落盘为止。有本地副本时 keepLocal 已覆盖（二者互斥），内容已在快照里时
  // 由上面的去重判定丢弃，故此注入不会与既有来源重复。
  const inflightQuery = snap.turn_in_flight ? snap.inflight_user_input : undefined;
  if (
    !keepLocal &&
    inflightQuery !== undefined &&
    lastCommittedUser !== inflightQuery
  ) {
    messages = [
      ...messages,
      { role: "user", content: inflightQuery, turnIndex: 0 },
    ];
  }

  set({
    messages,
    loaded: true,
    oldestLoadedTurn,
    hasMore,
    totalTurns: snap.total_turns ?? snap.turns.length,
    historyRevision: rev,
  });

  // 服务端有轮次在途 → 追加 assistant 占位并重新附着。
  // 占位是必须的：reduceStreamEvent 的 delta 只在末条是 assistant 时应用，
  // 且避免新轮次文本误并入上一轮最后一条已完成的 assistant 消息。
  // 判据用 turn_in_flight 而非 is_running —— 后者在 run 停靠等输入时
  // 同样为真，会挂出一条永不填充的空占位。
  const token = useAuthStore.getState().token;
  if (snap.turn_in_flight && token) {
    // 保留的本地尾部可能已带空占位（刚发那轮的 assistant 气泡）——
    // 重复追加会让 delta 落到错误的占位上。判据必须限定「无 id」：快照来源的
    // 空正文 assistant（如只带工具调用的那一轮）同样满足空正文，误判会让本轮的
    // delta 全落进那条已落盘的历史消息里。
    const cur = get().messages;
    const last = cur[cur.length - 1];
    const hasPlaceholder =
      last?.id === undefined && last.role === "assistant" && last.content === "";
    set({
      messages: hasPlaceholder
        ? cur
        : [...cur, { role: "assistant", content: "", turnIndex: 0 }],
      isStreaming: true,
    });
    openStream(pecoAttachUrl(), token, set, get);
  }
}

// 焦点刷新的在途去重：visibilitychange 与 focus 可能同刻触发两次
let refreshInFlight = false;

export const usePecoChatStore = create<PecoChatState>()((set, get) => ({
  loaded: false,
  loading: false,
  messages: [],
  sessionKey: 0,
  isStreaming: false,
  error: null,
  usage: null,
  oldestLoadedTurn: null,
  hasMore: false,
  totalTurns: 0,
  historyRevision: 0,
  loadingEarlier: false,

  load: async () => {
    if (get().loaded) return;
    set({ loading: true, error: null });
    try {
      await refreshSession(set, get);
    } catch {
      // 快照拉取失败：静默 — 首载无历史可显示，用户发消息即触发服务端构建
    } finally {
      set({ loading: false });
    }
  },

  refresh: async () => {
    // 本窗口正在流式 = 已实时，无需追平；快照重建还会丢进行中占位内容。
    // 不按「连接是否存在」短路 —— 连接常驻后那会让获焦追平永久失效。
    if (refreshInFlight || get().isStreaming) return;
    const token = useAuthStore.getState().token;
    if (!token) return;
    refreshInFlight = true;
    try {
      await refreshSession(set, get);
    } catch {
      // 追平失败不打断使用 — 保留现有消息，下次获焦重试
    } finally {
      refreshInFlight = false;
    }
  },

  clear: async () => {
    // Abort the in-flight stream before clearing, and cancel the server-side
    // run first (否则 runner 会在快照删除后仍按轮边界落盘).
    closeStream();
    try {
      await cancelPecoStream();
    } catch {
      // 无活跃任务时 404 — 常态，忽略
    }
    await clearPecoSession();
    selfHealUsed = false;
    set((s) => ({
      messages: [],
      loaded: false,
      isStreaming: false,
      usage: null,
      sessionKey: s.sessionKey + 1,
      oldestLoadedTurn: null,
      hasMore: false,
      totalTurns: 0,
      historyRevision: 0,
      loadingEarlier: false,
    }));
  },

  // ── loadEarlier ──────────────────────────────────────────────────────

  loadEarlier: async () => {
    const { isStreaming, hasMore, oldestLoadedTurn, totalTurns } = get();
    if (isStreaming || !hasMore || oldestLoadedTurn === null) return;

    set({ loadingEarlier: true });
    try {
      const snap = await getPecoSession({
        turns: PAGE_TURNS,
        before: oldestLoadedTurn,
      });
      if (get().isStreaming) return;

      const rev = snap.context_metrics?.compaction_count ?? 0;
      if (rev !== get().historyRevision) {
        // 历史版本已变（compaction）：放弃本次结果，重拉尾部页统一重建。
        await refreshSession(set, get);
        return;
      }

      // 版本未变：前置合并更早的轮（before 排他，返回轮 turnIndex 全 < oldestLoadedTurn）。
      const earlier = snapshotToMessages(snap.turns);
      const current = get().messages;
      // pinned 摘要分隔线恒在最前、恰好一条 —— 更早的轮插在它之后。
      const pinned =
        current.length > 0 && isPinnedNotice(current[0]) ? [current[0]] : [];
      const rest = current.slice(pinned.length);
      set({
        messages: [...pinned, ...earlier, ...rest],
        oldestLoadedTurn:
          snap.turns.length > 0 ? snap.turns[0].turn_index : oldestLoadedTurn,
        hasMore: snap.has_more ?? false,
        totalTurns: snap.total_turns ?? totalTurns,
      });
    } catch {
      // 加载更早失败：静默保留现状，按钮可重试。
    } finally {
      set({ loadingEarlier: false });
    }
  },

  // ── sendMessage ──────────────────────────────────────────────────────

  sendMessage: async (text: string, token: string) => {
    const state = get();
    if (state.isStreaming || !text.trim() || !token) return;

    // Append user message + empty assistant placeholder.
    const userMsg: ChatMessage = {
      role: "user",
      content: text,
      turnIndex: 0,
    };
    const assistantMsg: ChatMessage = {
      role: "assistant",
      content: "",
      turnIndex: 0,
    };
    set({
      messages: [...state.messages, userMsg, assistantMsg],
      isStreaming: true,
      error: null,
    });
    // 用户主动发起 → 重新武装一次性自愈
    selfHealUsed = false;

    // 已有活连接：复用（POST 投递），事件仍从这条连接回来
    if (conn && !conn.terminal) {
      try {
        await queryPecoStream(text);
        return;
      } catch (err) {
        const status = (err as { response?: { status?: number } })?.response
          ?.status;
        if (status !== 404) {
          // 背压（409）或网络错误：撤回占位气泡，保留用户消息供重发
          const messages = get().messages;
          const last = messages[messages.length - 1];
          set({
            messages:
              last?.role === "assistant" && last.content === ""
                ? messages.slice(0, -1)
                : messages,
            isStreaming: false,
            error: (err as Error)?.message ?? "消息发送失败，请重试",
          });
          return;
        }
        // 404：服务端已无该 run（连接已失效）→ 走引导路径重开
      }
    }

    // 无活连接（首条消息 / 连接已收尾 / POST 404）：带 message 开流，
    // 服务端自行决定附着到已有 run 还是新建。
    openStream(pecoStreamUrl(text), token, set, get, { replace: true });
  },

  // ── abortStream ──────────────────────────────────────────────────────

  abortStream: () => {
    // 服务端取消（fire-and-forget）：任务在途时 looper 于下个检查点收尾。
    void cancelPecoStream().catch(() => {});
    // 一并断开连接：被取消轮次的收尾事件（error）会晚于这次点击到达，
    // 若在此期间用户已开始下一轮，它会把新一轮的门控关掉。事件不带
    // 轮次标识，无法区分归属，断连是唯一干净的取舍（与取消前语义一致）。
    closeStream();
    set({ isStreaming: false });
  },

  clearError: () => {
    set({ error: null });
  },
}));

/** 仅供测试：重置模块级连接状态（setState 够不到模块作用域变量）。 */
export function __resetPecoStreamForTests(): void {
  closeStream();
  selfHealUsed = false;
}

// ── SSE Event Handler (store version) ────────────────────────────────────
//
// Thin wrapper around the shared reduceStreamEvent / isStreamTerminalEvent
// from ChatView.tsx.  Operates on Zustand's get() / set() instead of React's
// setMessages / setStreaming.

function applyStreamEvent(
  event: ChatSseEvent,
  set: (partial: Partial<PecoChatState>) => void,
  get: () => PecoChatState,
): void {
  // done = 服务端即将关闭该连接（run 收尾），标记后下次发送走引导路径
  if (event.event === "done" && conn) {
    conn.terminal = true;
  }

  // 门控：只在「本窗口领有在途轮次」时应用事件。连接是共享的广播扇出，
  // 其他窗口发起的轮次事件同样会到达本窗口 —— 不设门控就会把别人的文本
  // 追加进本窗口最后一条已完成的 assistant 气泡，或污染下一轮。
  if (!get().isStreaming) return;

  const newMessages = reduceStreamEvent(event, get().messages);

  // 捕获 ModelUsage 事件驱动用量圆环。仅 `usage` 事件携带「当前上下文
  // 窗口用量」（input_tokens）；done/turn_complete 的 usage 是会话累计量，
  // 不适合作为圆环分母。
  let usage: UsageData | null | undefined;
  if (event.event === "usage") {
    usage = {
      input_tokens: event.data.input_tokens,
      output_tokens: event.data.output_tokens,
    };
  }

  // Eagerly clear isStreaming on terminal events so the UI flips from
  // stop→send button immediately.
  if (isStreamTerminalEvent(event)) {
    set({
      messages: newMessages,
      isStreaming: false,
      ...(usage ? { usage } : {}),
    });
  } else {
    set({ messages: newMessages, ...(usage ? { usage } : {}) });
  }
}

interface PecoChatState {
  loaded: boolean;
  loading: boolean;
  messages: ChatMessage[];
  sessionKey: number;

  // Streaming state — managed by the store so it survives route navigation.
  // 语义是「本窗口领有一个在途轮次」，同时充当增量门控；不是「连接存在」。
  isStreaming: boolean;

  // Last error message from the streaming request (null when no error).
  error: string | null;

  // Current token usage for the context ring (null until the first stream event).
  usage: UsageData | null;

  // ── 分页状态（turn 级分页）──────────────────────────────────────────
  /** 已加载的最早轮位置号；null = 尚无已加载轮。 */
  oldestLoadedTurn: number | null;
  /** 是否还有更早的轮可加载（= 服务端窗口起点 > 0）。 */
  hasMore: boolean;
  /** 服务端报告的当前总轮数。 */
  totalTurns: number;
  /** 历史版本号（= context_metrics.compaction_count）；变了旧页即失效。 */
  historyRevision: number;
  /** 「加载更早」请求进行中（按钮置灰/加载态）。 */
  loadingEarlier: boolean;

  load: () => Promise<void>;
  /** 获焦/切回标签页时追平会话：重拉快照 + 检测在途轮次附着。 */
  refresh: () => Promise<void>;
  clear: () => Promise<void>;
  /** 拉取更早的一页（before=oldestLoadedTurn）并前置合并。 */
  loadEarlier: () => Promise<void>;

  /** Start an SSE streaming request. The async fetch runs inside the store
   *  and is NOT tied to any React component lifecycle. */
  sendMessage: (text: string, token: string) => Promise<void>;

  /** Abort the currently running SSE stream (server-side cancel + local). */
  abortStream: () => void;

  /** Clear the error state (call after displaying to user). */
  clearError: () => void;
}
