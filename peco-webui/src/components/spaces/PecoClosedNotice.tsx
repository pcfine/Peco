// Peco 空间未打开时的统一说辞：聊天列、文件模式、配置面的资源 tab 都摆这一条。
//
// 这三处的失败原因相同（空间没实例，请求会 409），但各自静默或各写一句话，
// 用户就要在三个地方分别猜自己是不是点错了。

import { EmptyState } from "@/components/common/EmptyState";
import { Button } from "@/components/ui/button";
import { useOpenSpace } from "@/components/spaces/useSpaceLifecycle";
import type { Space } from "@/types/daemon";

export function PecoClosedNotice({ space }: { space: Space }) {
  const { open, opening } = useOpenSpace(
    space.ws_id,
    space.work_dir,
    space.team_id,
  );

  return (
    <div className="p-3">
      <EmptyState
        title="Peco 空间还没打开，打开后才能和 Peco 对话、读写它的能力根"
        action={
          <Button size="sm" className="h-7" disabled={opening} onClick={open}>
            打开 Peco 空间
          </Button>
        }
      />
    </div>
  );
}
