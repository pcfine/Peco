const BY_EXTENSION: Record<string, string> = {
  py: "python",
  json: "json",
  sh: "bash",
  bash: "bash",
  zsh: "bash",
  yml: "yaml",
  yaml: "yaml",
  md: "markdown",
  markdown: "markdown",
  js: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  jsx: "javascript",
  ts: "typescript",
  tsx: "typescript",
  css: "css",
  html: "xml",
  xml: "xml",
  toml: "ini",
  ini: "ini",
  sql: "sql",
  rs: "rust",
  go: "go",
  java: "java",
  c: "c",
  h: "c",
  cpp: "cpp",
  txt: "plaintext",
};

/** 由相对路径推断 highlight.js 语言名；未知扩展名按纯文本处理。 */
export function languageOf(path: string): string {
  const name = path.split("/").pop() ?? path;
  const ext = name.includes(".") ? name.split(".").pop()!.toLowerCase() : "";
  return BY_EXTENSION[ext] ?? "plaintext";
}
