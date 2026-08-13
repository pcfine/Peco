use super::ToolError;
use peco_derive::peco_tool;

/// Shell command execution tool.
///
/// Executes a shell command via `sh -c` and returns the combined
/// stdout and stderr output.
#[peco_tool(
    name = "shell",
    description = "Execute a shell command and return the combined stdout and stderr output. Use this to run system commands, list directory contents, read files, or perform any other shell operation. The command is executed via `sh -c`. Set `cwd` to run the command from a specific directory.",
    params(
        command = "The shell command to execute. Examples: 'ls -la', 'cat file.txt', 'pwd', 'echo hello'",
        cwd = "Optional working directory to run the command from. Use an absolute path — e.g. a skill's directory (returned by read_skill) so relative script paths like 'python scripts/foo.py' resolve correctly."
    ),
    required = ["command"]
)]
pub async fn shell_exec(command: String, cwd: Option<String>) -> Result<String, ToolError> {
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c").arg(&command);
    if let Some(dir) = cwd.as_deref().filter(|d| !d.is_empty()) {
        cmd.current_dir(dir);
    }
    let output = cmd
        .output()
        .map_err(|e| ToolError::ToolCallError(Box::new(e)))?;

    let mut result = String::new();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !stdout.is_empty() {
        result.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str("STDERR:\n");
        result.push_str(&stderr);
    }
    if result.is_empty() {
        result = "(no output)".to_string();
    }

    Ok(result)
}
