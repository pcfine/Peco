import { z } from "zod";

import { DEFAULT_MODEL } from "./constants";

export const loginSchema = z.object({
  email: z.string().min(1, "请输入邮箱").email("邮箱格式不正确"),
  password: z.string().min(1, "请输入密码"),
});

export const registerSchema = z.object({
  username: z.string().min(1, "请输入用户名").min(2, "用户名至少 2 个字符"),
  email: z.string().min(1, "请输入邮箱").email("邮箱格式不正确"),
  password: z.string().min(6, "密码至少 6 个字符"),
});

export const agentSchema = z.object({
  name: z.string().min(1, "请输入 Agent 名称"),
  description: z.string().optional().default(""),
  system_prompt: z.string().optional().default(""),
  model: z.string().optional().default(DEFAULT_MODEL),
  provider: z.string().optional().default("deepseek"),
  icon: z.string().optional().default("🤖"),
  background_color: z.string().optional().default(""),
  tools: z.array(z.string()).optional().default([]),
  skills: z.array(z.string()).optional().default([]),
  knowledge_bases: z.array(z.string()).optional().default([]),
  mcp_servers: z.array(z.string()).optional().default([]),
  temperature: z.number().min(0).max(2).optional(),
  max_tokens: z.number().positive().optional(),
  reasoning_effort: z.string().optional(),
  max_iterations: z.number().positive().optional(),
});

// 与后端 validate_kb_name 对齐
export const kbNameSchema = z
  .string()
  .min(1, "请输入知识库名称")
  .max(64, "名称最多 64 个字符")
  .regex(
    /^[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?$/,
    "名称需以字母或数字开头和结尾，且只能包含字母、数字、.、_、-（不能以 @ 开头）",
  );

export const knowledgeBaseSchema = z.object({
  name: kbNameSchema,
  description: z.string().optional().default(""),
});
