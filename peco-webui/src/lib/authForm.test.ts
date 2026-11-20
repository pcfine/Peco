import { describe, expect, it } from "vitest";
import {
  confirmPasswordError,
  newPasswordError,
  setupTokenError,
  usernameError,
} from "@/lib/authForm";

// 注册页与设置页共用这套规则：两边各写一份就会有一边悄悄松掉。

describe("账号表单校验", () => {
  it("用户名去空白后不能为空", () => {
    expect(usernameError("   ")).toBe("用户名不能为空");
    expect(usernameError(" a ")).toBeNull();
  });

  it("用户名上限 64 个字符（按去空白后计）", () => {
    expect(usernameError("a".repeat(64))).toBeNull();
    expect(usernameError("a".repeat(65))).toBe("用户名不能超过 64 个字符");
    // 首尾空白不算进长度：粘进来的尾随空格不该把合法名字顶成非法。
    expect(usernameError(` ${"a".repeat(64)} `)).toBeNull();
  });

  it("密码长度 8..128", () => {
    expect(newPasswordError("a".repeat(7))).toBe("密码要 8 到 128 个字符");
    expect(newPasswordError("a".repeat(8))).toBeNull();
    expect(newPasswordError("a".repeat(128))).toBeNull();
    expect(newPasswordError("a".repeat(129))).toBe("密码要 8 到 128 个字符");
  });

  it("两次输入必须一致", () => {
    expect(confirmPasswordError("password1", "password2")).toBe(
      "两次输入的密码不一致",
    );
    expect(confirmPasswordError("password1", "password1")).toBeNull();
  });

  it("setup token 不能只填空格", () => {
    expect(setupTokenError("  ")).toBe("请粘贴 setup token");
    expect(setupTokenError("0".repeat(64))).toBeNull();
  });
});
