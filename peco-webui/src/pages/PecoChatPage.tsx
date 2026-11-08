// `/peco` —— Peco 回到主区：左聊天、右栏四模式（默认总览），再点一次翻到能力配置面。
//
// 翻转只作用于主内容区：页头留在外面，「返回聊天」永远点得到。两个面常驻 ——
// 聊天面被翻走时聊天壳仍挂着，在途那一轮不会因为卸载被打断。
//
// 配置面的资源 tab 要一个已打开的 Peco 空间作 `X-Peco-Space`；空间没打开时
// 三个位置（聊天列、文件模式、资源 tab）统一摆 `PecoClosedNotice`，不各写一句。

import { useEffect, useState, type ReactNode } from "react";
import type { UseQueryResult } from "@tanstack/react-query";
import { useSearchParams } from "react-router-dom";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { PecoClosedNotice } from "@/components/spaces/PecoClosedNotice";
import { SpaceChatShell } from "@/components/spaces/SpaceChatShell";
import { SpacePanelModeBar } from "@/components/spaces/SpacePanelModeBar";
import { SpaceSidePanel } from "@/components/spaces/SpaceSidePanel";
import {
  SPACE_PANEL_MODES,
  useSpacePanelState,
} from "@/components/spaces/useSpacePanelState";
import { TeamConfigPanel } from "@/components/teams/TeamConfigPanel";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { useMediaQuery } from "@/hooks/useMediaQuery";
import { errorText } from "@/lib/api";
import { TASK_RUNNING_STATES } from "@/lib/taskStates";
import { cn } from "@/lib/utils";
import { usePeco } from "@/queries/peco";
import { useSpaces } from "@/queries/spaces";
import { usePecoTodayTasks } from "@/queries/tasks";
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

  const spaces = useSpaces();
  const space =
    (spaces.data ?? []).find((item) => item.ws_id === peco.ws_id) ?? null;
  const spaceOpen = space?.status === "open";

  const panel = useSpacePanelState(peco.ws_id, "peco", {
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
      <header className="shrink-0 border-b border-line px-3 py-3">
        <div className="flex h-8 items-center gap-2">
          <h1 className="min-w-0 truncate text-xl font-semibold text-ink">
            Peco
          </h1>
          <span className="shrink-0 font-mono text-2xs text-ink-faint">
            {peco.ws_id}
          </span>
          {space && (
            <span className="shrink-0 text-caption font-medium text-ink-faint">
              {spaceOpen ? "进行中" : "未打开"}
            </span>
          )}
          <div className="ml-auto flex shrink-0 items-center gap-1">
            {face === "chat" && (
              <SpacePanelModeBar
                mode={panel.mode}
                expanded={!panel.collapsed}
                inflight={inflight}
                modes={SPACE_PANEL_MODES.peco}
                onSelect={panel.selectMode}
                onToggle={panel.toggleCollapsed}
              />
            )}
            <Button
              variant="ghost"
              size="sm"
              className="h-7"
              onClick={toggleFace}
            >
              {face === "chat" ? "能力配置" : "返回聊天"}
            </Button>
          </div>
        </div>
        <div className="mt-1 flex h-5 min-w-0 items-center gap-2 text-caption font-medium text-ink-faint">
          <span
            className="truncate font-mono text-mono-sm"
            title={peco.work_dir}
          >
            {peco.work_dir}
          </span>
          <span className="shrink-0">·</span>
          <span className="shrink-0">团队 {peco.team_id}</span>
        </div>
      </header>

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
            <div className="flex min-h-0 min-w-0 flex-1 flex-col p-3">
              {unavailable ?? (
                <SpaceChatShell wsId={peco.ws_id} visible={face === "chat"} />
              )}
            </div>

            {!panel.collapsed && (
              <aside
                data-testid="peco-panel"
                data-mode={panel.mode}
                aria-label="Peco 右栏"
                className="flex w-80 shrink-0 flex-col border-l border-line bg-side"
              >
                <div className="min-h-0 flex-1">
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
              "absolute inset-0 mx-auto flex min-h-0 w-full max-w-wide flex-col",
              motion === "flip" &&
                "[backface-visibility:hidden] [transform:rotateY(180deg)]",
              faceFadeClasses(face === "config"),
            )}
          >
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
  );
}
