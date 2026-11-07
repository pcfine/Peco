-- 021: 每个空间每个 agent 只有一条 scope='space' 会话
-- 空间页是一条 @master 对话：没有这条部分唯一索引，两个并发的 ensure 请求
-- 会各插一行，之后走哪一份历史就取决于列表顺序。
-- 只约束 scope='space'：普通 agent 会话（scope='agent'）本来就允许并列多条。
-- 注意：注释内禁用半角分号（迁移按半角分号拆分语句执行）。
-- 不给历史行去重：scope='space' 是本次改动新引入的取值，库里不可能有重复行；
-- 真要撞键说明有人手改过库，让迁移失败比静默删掉一行更安全。
CREATE UNIQUE INDEX IF NOT EXISTS idx_conversations_space_unique
  ON conversations(workspace_id, agent_name) WHERE scope = 'space'
