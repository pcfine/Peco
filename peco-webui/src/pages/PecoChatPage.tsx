// `/peco` —— Peco 回到主区：左聊天、右栏四模式（默认总览），再点一次翻到能力配置面。
//
// 聊天面与空间页同构：两块各占一半，两侧头部各留一条「展开」的回来路径，不再有横跨
// 整宽的页头。两个面常驻 —— 聊天面被翻走时聊天壳仍挂着，在途那一轮不会因为卸载被打断；
// 配置面自带一条头部，翻过去之后「返回聊天」永远点得到。
//
// 配置面的资源 tab 要一个已打开的 Peco 空间作 `X-Peco-Space`；空间没打开时
// 三个位置（聊天列、文件模式、资源 tab）统一摆 `PecoClosedNotice`，不各写一句。

import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import type { UseQueryResult } from "@tanstack/react-query";
import { ArrowLeft, Settings2 } from "lucide-react";
import { useSearchParams } from "react-router-dom";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { ClearConversationDialog } from "@/components/spaces/ClearConversationDialog";
import { PecoClosedNotice } from "@/components/spaces/PecoClosedNotice";
import { SpaceChatHeader } from "@/components/spaces/SpaceChatHeader";
import { SpaceChatShell } from "@/components/spaces/SpaceChatShell";
import { SpaceHeaderIconButton } from "@/components/spaces/SpaceHeaderIconButton";
import { SpacePanelHeader } from "@/components/spaces/SpacePanelHeader";
import { SpaceSidePanel } from "@/components/spaces/SpaceSidePanel";
import { useConversationClear } from "@/components/spaces/useConversationClear";
import {
  SPACE_PANEL_MODES,
  useSpacePanelState,
} from "@/components/spaces/useSpacePanelState";
import { WORK_INLINE_MIN, workWidthOf } from "@/components/layout/shellMetrics";
import { TeamConfigPanel } from "@/components/teams/TeamConfigPanel";
import { Skeleton } from "@/components/ui/skeleton";
import { useMediaQuery } from "@/hooks/useMediaQuery";
import { useViewportWidth } from "@/hooks/useViewportWidth";
import { errorText } from "@/lib/api";
import { TASK_RUNNING_STATES } from "@/lib/taskStates";
import { cn } from "@/lib/utils";
import { usePeco } from "@/queries/peco";
import { useSpaces } from "@/queries/spaces";
import { usePecoTodayTasks } from "@/queries/tasks";
import { useUiStore } from "@/stores/uiStore";
import type { PecoIdentity, Space } from "@/types/daemon";

/** Peco 是后端保留团队：配置面的 teamId 只能是它，不跟着路由参数走。 */
const PECO_TEAM_ID = "peco";

/** 翻转与淡入的两档时长；锁在动画结束后再留一点余量。 */
const FLIP_MS = { flip: 400, fade: 120 } as const;

type Face = "chat" | "config";

export function PecoChatPage() {
  const peco = usePeco();

  if (peco.isPending) {
    return (
      <div className="space-y-px">
        {[0, 1, 2].map((row) => (
          <Skeleton key={row} className="h-10 w-full" />
        ))}
      </div>
    );
  }

  if (peco.error || !peco.data) {
    return (
      <ErrorBanner
        message={errorText(peco.error)}
        onRetry={() => void peco.refetch()}
      />
    );
  }

  return <PecoWorkspace peco={peco.data} />;
}

/** 空间清单还没到手、拉取失败或未打开时，聊天列与文件面板共用这一块。 */
function spaceUnavailableNotice(
  space: Space | null,
  open: boolean,
  spaces: UseQueryResult<Space[], Error>,
): ReactNode {
  if (open) return null;
  if (spaces.error) {
    return (
      <div className="p-3">
        <ErrorBanner
          message={errorText(spaces.error)}
          onRetry={() => void spaces.refetch()}
        />
      </div>
    );
  }
  if (!space) {
    return (
      <div className="space-y-2 p-3">
        <Skeleton className="h-10 w-full" />
        <Skeleton className="h-10 w-full" />
      </div>
    );
  }
  return <PecoClosedNotice space={space} />;
}

type HeaderSpace = Pick<
  Space,
  "label" | "work_dir" | "team_id" | "status" | "closed_at"
>;

/**
 * 左头要一个 `Space` 才能画状态点与标题。空间清单还没到手时用身份顶上：
 * 全页唯一的 h1 与三个动作都长在这一行，缺了它加载窗口里就没有标题。
 * 状态按「已存在」算（`closed` 且没被显式关过）—— 那时确实还没有实例。
 */
function headerSpaceOf(peco: PecoIdentity, space: Space | null) {
  if (space) return space;
  return {
    label: "Peco",
    work_dir: peco.work_dir,
    team_id: peco.team_id,
    status: "closed",
    closed_at: null,
  } satisfies HeaderSpace;
}

function PecoWorkspace({ peco }: { peco: PecoIdentity }) {
  const [searchParams, setSearchParams] = useSearchParams();
  const face: Face = searchParams.get("face") === "config" ? "config" : "chat";

  // 减弱动效时不转 3D，改成 120ms 淡入淡出；两个面始终在文档里。
  const reduceMotion = useMediaQuery("(prefers-reduced-motion: reduce)");
  const motion: "flip" | "fade" = reduceMotion ? "fade" : "flip";
  const [animating, setAnimating] = useState(false);

  // URL 是「哪一面」的唯一来源：左栏的 Peco 行也直接改它，不经过本页的按钮。
  // 首帧不算换面 —— 直达 `?face=config` 时终态就是初始态，不播入场动画。
  const [shownFace, setShownFace] = useState<Face>(face);
  if (shownFace !== face) {
    setShownFace(face);
    setAnimating(true);
  }

  const viewport = useViewportWidth();
  const railOpen = useUiStore((state) => state.railOpen);
  // 与空间页、壳的栅格同源：work 均分后每块不足 412 时，两块互斥且聊天优先。
  const exclusive = workWidthOf(viewport, railOpen) < WORK_INLINE_MIN;

  const spaces = useSpaces();
  const space =
    (spaces.data ?? []).find((item) => item.ws_id === peco.ws_id) ?? null;
  const spaceOpen = space?.status === "open";

  const panel = useSpacePanelState(peco.ws_id, "peco", {
    exclusive,
    modeFromUrl: searchParams.get("panel"),
    // 只改 `panel`：`face` 决定的是另一件事，切模式不该把它带掉。replace 不加历史条目。
    onModeChange: (mode) =>
      setSearchParams(
        (previous) => {
          const next = new URLSearchParams(previous);
          next.set("panel", mode);
          return next;
        },
        { replace: true },
      ),
  });
  const tasks = usePecoTodayTasks(peco.ws_id);
  const inflight = (tasks.data ?? []).filter((task) =>
    TASK_RUNNING_STATES.includes(task.task_state),
  ).length;

  const unavailable = spaceUnavailableNotice(space, spaceOpen, spaces);

  // 哪个空间的对话正在跑。`connectionStore.chat` 是全局一份，拿它判断本页会把
  // 别的空间的「进行中」算到自己头上：这里按 wsId 记一位，与空间页同一套写法。
  const [streamingWs, setStreamingWs] = useState<Record<string, boolean>>({});
  const setWsStreaming = useCallback((id: string, streaming: boolean) => {
    setStreamingWs((previous) =>
      previous[id] === streaming ? previous : { ...previous, [id]: streaming },
    );
  }, []);

  // 空间没打开时不发 ensure：先去「打开」，别让用户在清除对话上先吃一个 409。
  const clear = useConversationClear(
    peco.ws_id,
    streamingWs[peco.ws_id] ?? false,
    { enabled: spaceOpen },
  );

  const expandChatRef = useRef<HTMLButtonElement>(null);
  const expandPanelRef = useRef<HTMLButtonElement>(null);
  const toggledByUser = useRef(false);

  // 折叠把被点的那枚按钮一起带走，焦点会掉回 body：落到对面刚出现的「展开」上，
  // 读屏也就跟着念出落点。两块都在时无需移动。
  useEffect(() => {
    if (!toggledByUser.current) return;
    toggledByUser.current = false;
    const target = panel.chatCollapsed
      ? expandChatRef
      : panel.collapsed
        ? expandPanelRef
        : null;
    target?.current?.focus();
  }, [panel.chatCollapsed, panel.collapsed]);

  const toggleChat = () => {
    toggledByUser.current = true;
    panel.toggleChat();
  };

  const togglePanel = () => {
    toggledByUser.current = true;
    panel.toggleCollapsed();
  };

  useEffect(() => {
    if (!animating) return;
    // 锁跟着时长走而不是跟 transitionend 走：减弱动效、标签页切走都可能收不到
    // 事件，锁一旦等不到就会把按钮永久钉死。
    const timer = setTimeout(() => setAnimating(false), FLIP_MS[motion] + 60);
    return () => clearTimeout(timer);
  }, [animating, motion]);

  const toggleFace = () => {
    // 翻转途中再点会从半途反向，观感是抖动而不是翻转；锁到这一程走完。
    if (animating) return;
    setSearchParams((previous) => {
      const next = new URLSearchParams(previous);
      if (face === "chat") next.set("face", "config");
      else next.delete("face");
      return next;
    });
  };

  const faceFadeClasses = (visible: boolean) =>
    motion === "fade" &&
    cn(
      animating && "peco-flip-fade transition-opacity",
      !visible && "pointer-events-none opacity-0",
    );

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div
        data-testid="peco-flip"
        data-face={face}
        data-motion={motion}
        data-animating={animating ? "true" : "false"}
        aria-busy={animating}
        className="relative min-h-0 flex-1 [perspective:1600px]"
      >
        <div
          className={cn(
            "relative h-full min-h-0",
            motion === "flip" && "[transform-style:preserve-3d]",
            motion === "flip" &&
              (face === "config"
                ? "[transform:rotateY(180deg)]"
                : "[transform:rotateY(0deg)]"),
            motion === "flip" &&
              animating &&
              "transition-transform duration-[400ms] ease-[cubic-bezier(.2,.8,.2,1)]",
          )}
        >
          <div
            data-testid="peco-face-chat"
            inert={face !== "chat"}
            aria-hidden={face !== "chat"}
            className={cn(
              "absolute inset-0 flex min-h-0 min-w-0",
              motion === "flip" && "[backface-visibility:hidden]",
              faceFadeClasses(face === "chat"),
            )}
          >
            <div
              className={cn(
                "min-w-0 flex-1 basis-0 flex-col",
                panel.chatCollapsed ? "hidden" : "flex",
              )}
            >
              <SpaceChatHeader
                space={headerSpaceOf(peco, space)}
                panelCollapsed={panel.collapsed}
                onExpandPanel={togglePanel}
                onCollapseChat={toggleChat}
                expandPanelRef={expandPanelRef}
                extraActions={
                  <SpaceHeaderIconButton
                    label="能力配置"
                    icon={Settings2}
                    onClick={toggleFace}
                  />
                }
                onClearConversation={clear.openDialog}
                clearDisabled={clear.disabled}
                clearDisabledReason={clear.disabledReason}
                title="Peco"
                titleHint={peco.work_dir}
              />
              <div className="flex min-h-0 flex-1 flex-col px-4 pb-3">
                {unavailable ?? (
                  <SpaceChatShell
                    wsId={peco.ws_id}
                    visible={face === "chat"}
                    onStreamingChange={setWsStreaming}
                  />
                )}
              </div>
            </div>

            {!panel.collapsed && (
              <aside
                data-testid="peco-panel"
                data-mode={panel.mode}
                aria-label="Peco 右栏"
                className="flex min-w-0 flex-1 basis-0 flex-col border-l border-line bg-side"
              >
                <SpacePanelHeader
                  mode={panel.mode}
                  modes={SPACE_PANEL_MODES.peco}
                  inflight={inflight}
                  chatCollapsed={panel.chatCollapsed}
                  standalone={panel.chatCollapsed}
                  onSelect={panel.selectMode}
                  onExpandChat={toggleChat}
                  onCollapsePanel={togglePanel}
                  expandChatRef={expandChatRef}
                />
                <div
                  className={cn(
                    "@container/panel min-h-0 flex-1",
                    panel.chatCollapsed && "mx-auto w-full max-w-mid px-3",
                  )}
                >
                  {panel.mode === "files" && unavailable ? (
                    unavailable
                  ) : (
                    <SpaceSidePanel
                      wsId={peco.ws_id}
                      mode={panel.mode}
                      variant="peco"
                      active={face === "chat"}
                    />
                  )}
                </div>
              </aside>
            )}
          </div>

          <div
            data-testid="peco-face-config"
            inert={face !== "config"}
            aria-hidden={face !== "config"}
            className={cn(
              "absolute inset-0 flex min-h-0 w-full flex-col",
              motion === "flip" &&
                "[backface-visibility:hidden] [transform:rotateY(180deg)]",
              faceFadeClasses(face === "config"),
            )}
          >
            <header className="flex h-12 shrink-0 items-center border-b border-line px-4">
              <h2 className="min-w-0 truncate text-xl font-semibold text-ink">
                能力配置
              </h2>
              <div className="ml-auto flex shrink-0 items-center gap-0.5">
                <SpaceHeaderIconButton
                  label="返回聊天"
                  icon={ArrowLeft}
                  onClick={toggleFace}
                />
              </div>
            </header>
            <div className="min-h-0 flex-1">
              <TeamConfigPanel
                teamId={PECO_TEAM_ID}
                wsId={spaceOpen ? peco.ws_id : null}
                headingLevel="h2"
                hideSpaces
                hideAssemble
                noSpaceNotice={unavailable}
                // 空间没打开时也传：记忆面板自己会摆「打开空间」的入口，
                // 比团队维度的「尚未接入」占位更靠近用户想做的那件事。
                memoryWsId={peco.ws_id}
              />
            </div>
          </div>
        </div>
      </div>

      <ClearConversationDialog
        open={clear.dialogOpen}
        pending={clear.pending}
        onCancel={clear.cancel}
        onConfirm={clear.confirm}
      />
    </div>
  );
}
