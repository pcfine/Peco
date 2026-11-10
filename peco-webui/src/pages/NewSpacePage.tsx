// `/spaces/new` —— 两块同时可见，不用步骤条、不加序号（序号是装饰）。
// 提交成功直接进空间详情：向导的下一个动作就是「看它跑起来」。
//
// 表单本体在 `NewSpaceForm`：团队配置页的「绑定空间」tab 共用它。

import { useNavigate, useSearchParams } from "react-router-dom";
import { NewSpaceForm } from "@/components/spaces/NewSpaceForm";

export function NewSpacePage() {
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();

  return (
    <div className="mx-auto w-full max-w-mid space-y-6">
      <h1 className="text-xl font-semibold text-ink">新建空间</h1>
      <p className="max-w-narrow text-sm text-ink-muted">
        空间 = 一个目录 + 一个团队。目录里的 `.workspace.json` 记录它的身份，
        团队决定它能用哪些 agent 与 skill。
      </p>

      <NewSpaceForm
        initialTeamId={searchParams.get("team") ?? ""}
        onCreated={(wsId) => navigate(`/spaces/${encodeURIComponent(wsId)}`)}
      />
    </div>
  );
}
