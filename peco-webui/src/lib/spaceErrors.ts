// 新建向导与服务端错误码之间的翻译层：每个 code 给一句「下一步该做什么」，
// 而不是把 `work_dir_unusable` 这样的名字原样丢给用户。
//
// 单独成文件是为了可测：这些文案是这一页唯一的用户可见承诺。

import { ApiError, errorText } from "@/lib/api";

const HINTS: Record<string, string> = {
  work_dir_not_absolute: "目录要写绝对路径，例如 /home/you/work/my-space。",
  work_dir_unusable: "这个目录建不出来（权限或路径不对），换一个位置试试。",
  work_dir_missing: "这个目录已经不存在了，先确认它还在磁盘上。",
  team_missing:
    "这个团队在 daemon 的 teams/ 下没有目录，换一个团队或先建团队。",
  team_conflict:
    "这个目录已经绑定了另一个团队；同一个目录不能同时属于两个团队。",
  team_exists: "这个团队 id 已经被占用，换一个 id。",
  team_path_mismatch:
    "目录里的 .workspace.json 记的团队路径与 daemon 的团队根不一致。",
  marker_db_conflict:
    "目录里的 .workspace.json 与索引记的会话不一致，需要人工确认哪一个是对的。",
  marker_corrupt: "目录里的 .workspace.json 读不出来，修好或移走它再试。",
  marker_schema_unsupported:
    "目录里的 .workspace.json 是更新版本的格式，当前 daemon 读不了。",
  marker_write_failed:
    "记录文件写不进去（磁盘或权限），目录已被创建但没有标记。",
  invalid_argument: "请求参数不合法。",
  space_not_found: "空间没建成，刷新后再试。",
};

/** 服务端 code → 可操作文案；没有对应 code 时退回服务端 message。 */
export function spaceErrorText(error: unknown): string {
  if (error instanceof ApiError && error.code) {
    const hint = HINTS[error.code];
    if (hint) return hint;
  }
  return errorText(error);
}
