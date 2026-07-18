import { z } from 'zod'

export const loginSchema = z.object({
  email: z.string().min(1, '请输入邮箱').email('邮箱格式不正确'),
  password: z.string().min(1, '请输入密码'),
})

export const registerSchema = z.object({
  username: z.string().min(1, '请输入用户名').min(2, '用户名至少 2 个字符'),
  email: z.string().min(1, '请输入邮箱').email('邮箱格式不正确'),
  password: z.string().min(6, '密码至少 6 个字符'),
})

export const agentSchema = z.object({
  name: z.string().min(1, '请输入 Agent 名称'),
  description: z.string().optional().default(''),
  system_prompt: z.string().optional().default(''),
  model: z.string().optional().default('deepseek-v4-flash'),
  provider: z.string().optional().default('deepseek'),
  icon: z.string().optional().default('🤖'),
  color: z.string().optional().default('#6366f1'),
  temperature: z.number().min(0).max(2).optional(),
  max_tokens: z.number().positive().optional(),
})

export const knowledgeBaseSchema = z.object({
  name: z.string().min(1, '请输入知识库名称'),
  description: z.string().optional().default(''),
})
