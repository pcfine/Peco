// Provider 层的共用能力 —— 目前只有连接探针。
//
// 探针不属于任何传输层：peco-server 的 `/api/providers/test` 与 daemon 的
// 团队级端点测的是同一条适配器分派路径，放在 HTTP 层会让两侧各有一份实现。

pub mod probe;
