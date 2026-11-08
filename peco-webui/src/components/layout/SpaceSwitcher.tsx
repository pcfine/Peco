// 空间切换的唯一入口：rail 总线行与窄屏顶栏都走这里，状态只有一份（spaceStore）。

import { useNavigate } from "react-router-dom";
import { ChevronsUpDown } from "lucide-react";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { spaceLabel } from "@/components/layout/spaceLabel";
import { useSpaces } from "@/queries/spaces";
import { useSpaceStore } from "@/stores/spaceStore";
import { useSwitchSpace } from "@/hooks/useSwitchSpace";

export function SpaceSwitcher() {
  const navigate = useNavigate();
  const { data: spaces } = useSpaces();
  const currentWsId = useSpaceStore((state) => state.currentWsId);
  const switchSpace = useSwitchSpace();

  const current = spaces?.find((space) => space.ws_id === currentWsId);

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          className="flex h-7 max-w-[220px] items-center gap-1.5 rounded-[6px] px-2 text-sm text-ink hover:bg-line"
          aria-label="切换当前空间"
        >
          <span className="truncate">
            {current ? spaceLabel(current) : "选择空间"}
          </span>
          <ChevronsUpDown className="h-3.5 w-3.5 shrink-0 text-ink-faint" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="w-64">
        <DropdownMenuLabel className="text-caption font-medium text-ink-faint">
          当前空间
        </DropdownMenuLabel>
        <DropdownMenuRadioGroup
          value={currentWsId ?? undefined}
          onValueChange={switchSpace}
        >
          {(spaces ?? []).map((space) => (
            <DropdownMenuRadioItem
              key={space.ws_id}
              value={space.ws_id}
              className="text-sm"
            >
              <span className="flex w-full min-w-0 items-center gap-2">
                <span className="truncate">{spaceLabel(space)}</span>
                <span className="ml-auto shrink-0 font-mono text-2xs text-ink-faint">
                  {space.team_id}
                </span>
              </span>
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
        {spaces && spaces.length === 0 && (
          <p className="px-2 py-1.5 text-xs text-ink-faint">还没有工作空间。</p>
        )}
        <DropdownMenuSeparator />
        <DropdownMenuItem
          className="text-xs text-ink-muted"
          onSelect={() => navigate("/spaces")}
        >
          管理空间
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
