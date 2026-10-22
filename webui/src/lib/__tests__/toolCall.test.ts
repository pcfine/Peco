// 工具卡片的纯逻辑：截断、摘要提取、轮末 pending 结算

import { describe, expect, it } from "vitest";
import {
  SUMMARY_CHAR_CAP,
  capText,
  settlePendingToolCalls,
  summarizeArguments,
  type ToolCallView,
} from "../toolCall";

/** settle 的宿主最小形状 —— 它不该依赖 ChatMessage。 */
interface Row {
  content?: string;
  toolCalls?: ToolCallView[];
}

const call = (
  id: string,
  status: ToolCallView["status"],
  extra: Partial<ToolCallView> = {},
): ToolCallView => ({ id, name: "shell", arguments: "{}", status, ...extra });

describe("capText", () => {
  it("恰好等于上限时原样返回，不标记截断", () => {
    expect(capText("abc", 3)).toEqual({
      text: "abc",
      truncated: false,
      total: 3,
    });
  });

  it("超限时截到上限并补省略号，total 保留原长", () => {
    const capped = capText("abcdef", 3);
    expect(capped.text).toBe("abc…");
    expect(capped.truncated).toBe(true);
    expect(capped.total).toBe(6);
  });

  it("空串不算截断", () => {
    expect(capText("", 10)).toEqual({ text: "", truncated: false, total: 0 });
  });
});

describe("summarizeArguments", () => {
  it("command 优先于 id：摘命令而不是模型自造的调用 id", () => {
    expect(summarizeArguments('{"command":"ls -l","id":"x"}')).toBe("ls -l");
  });

  it("候选取值按优先级：query 压过 path 与 name", () => {
    expect(
      summarizeArguments('{"name":"n","path":"/tmp/a","query":"find it"}'),
    ).toBe("find it");
  });

  it("委派类工具摘 agent_name 而非冗长的 prompt", () => {
    expect(
      summarizeArguments(
        '{"agent_name":"dev-agent","prompt":"很长的任务描述，读代码、改实现、跑测试并汇报"}',
      ),
    ).toBe("dev-agent");
  });

  it("只有 prompt 时退回提示词本身", () => {
    expect(summarizeArguments('{"prompt":"只有提示词"}')).toBe("只有提示词");
  });

  it("只有 id 时返回空串 —— 不兜底猜值", () => {
    expect(summarizeArguments('{"id":"abc"}')).toBe("");
  });

  it("null / 对象 / 数组值跳过，继续看后面的键", () => {
    expect(
      summarizeArguments(
        '{"command":null,"cmd":{"a":1},"query":["x"],"q":"ok"}',
      ),
    ).toBe("ok");
  });

  it("空白值视为未命中，继续看后面的键", () => {
    expect(summarizeArguments('{"command":"   ","cmd":"ls"}')).toBe("ls");
  });

  it("数字与布尔值也可当摘要", () => {
    expect(summarizeArguments('{"q":42}')).toBe("42");
    expect(summarizeArguments('{"q":true}')).toBe("true");
  });

  it("折掉换行与连续空白 —— 折叠行只有一行", () => {
    expect(summarizeArguments('{"command":"ls\\n\\n   -l"}')).toBe("ls -l");
  });

  it("非 JSON 退回去空白化后的原串，不抛错也不返回空串", () => {
    expect(summarizeArguments("{bad  json}")).toBe("{bad json}");
  });

  it("非对象 JSON 同样退回去空白化后的原串", () => {
    expect(summarizeArguments("null")).toBe("null");
    expect(summarizeArguments("[1,2]")).toBe("[1,2]");
    expect(summarizeArguments('"raw"')).toBe('"raw"');
  });

  it("空串与空对象都没有摘要", () => {
    expect(summarizeArguments("")).toBe("");
    expect(summarizeArguments("{}")).toBe("");
  });

  it("超长摘要截到 80 字符", () => {
    const long = "x".repeat(SUMMARY_CHAR_CAP + 5);
    expect(summarizeArguments(JSON.stringify({ command: long }))).toBe(
      `${"x".repeat(SUMMARY_CHAR_CAP)}…`,
    );
  });
});

describe("settlePendingToolCalls", () => {
  it("扫全部消息：末尾追加的错误气泡不会挡住前面的 pending", () => {
    const messages: Row[] = [
      { toolCalls: [call("a", "pending")] },
      { content: "模型报错", toolCalls: [call("b", "done")] },
    ];

    const settled = settlePendingToolCalls(messages);

    expect(settled[0].toolCalls?.[0].status).toBe("missing");
    // 已结算的不动
    expect(settled[1].toolCalls?.[0].status).toBe("done");
  });

  it("一个宿主里的多个 pending 全部结算", () => {
    const messages: Row[] = [
      { toolCalls: [call("a", "pending"), call("b", "pending")] },
    ];
    expect(
      settlePendingToolCalls(messages)[0].toolCalls?.map((tc) => tc.status),
    ).toEqual(["missing", "missing"]);
  });

  it("没有 pending 时返回原引用，避免无谓重渲染", () => {
    const messages: Row[] = [
      { toolCalls: [call("a", "done"), call("b", "missing")] },
      { content: "正文" },
    ];
    expect(settlePendingToolCalls(messages)).toBe(messages);
  });

  it("无 toolCalls 的消息原样保留", () => {
    const messages: Row[] = [{ content: "只有正文" }];
    expect(settlePendingToolCalls(messages)).toBe(messages);
  });
});
