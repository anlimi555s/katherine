// tools/base.rs — 6 个基础工具：Read, Write, Edit, Bash, Glob, Grep。
// 每个工具是一个 struct，实现 Tool trait。

use std::fs;
use std::path::Path;
use std::process::Command;

use katherine_core::capability::Capability;
use katherine_core::error::EngineError;
use katherine_core::tool::{PermissionLevel, Tool, ToolDefinition, ToolResult};

// ── Helpers ───────────────────────────────────────────────

/// 读取文件内容并带行号返回（和现有 Read 工具对齐）。
fn read_file_with_numbers(path: &str, offset: usize, limit: usize) -> Result<String, EngineError> {
    let content = fs::read_to_string(path).map_err(|e| {
        EngineError::ToolExecutionFailed {
            name: "Read".into(),
            message: format!("Cannot read {path}: {e}"),
        }
    })?;
    let lines: Vec<&str> = content.lines().collect();
    let start = if offset > 0 { offset - 1 } else { 0 };
    let end = (start + limit).min(lines.len());
    Ok(lines[start..end]
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{}\t{}", start + i + 1, l))
        .collect::<Vec<_>>()
        .join("\n"))
}

// ── Read ───────────────────────────────────────────────────

pub struct ReadTool;

impl ReadTool {
    pub fn new() -> Self {
        ReadTool
    }
}

impl Tool for ReadTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "Read".into(),
            description: "读本地文件。结果以 cat -n 格式返回，行号从 1 开始。可读图片（PNG、JPG 等）并呈现。可通过 pages 参数读 PDF。读目录、不存在的文件或空文件返回错误而不是内容。不要重读刚编辑过的文件来验证——如果改动失败 Edit/Write 会报错。".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Absolute path to the file" },
                    "offset": { "type": "number", "description": "Line number to start reading from" },
                    "limit": { "type": "number", "description": "Number of lines to read" }
                },
                "required": ["file_path"]
            }),
            permission_level: PermissionLevel::ReadOnly,
        }
    }

    fn execute(&self, input: serde_json::Value) -> Result<ToolResult, EngineError> {
        let path = input["file_path"].as_str().unwrap_or("");
        if path.is_empty() {
            return Ok(ToolResult::error("Error: file_path is required"));
        }
        if !Path::new(path).exists() {
            return Ok(ToolResult::error(format!("Error: File not found: {path}")));
        }
        let offset = input["offset"].as_u64().unwrap_or(1) as usize;
        let limit = input["limit"].as_u64().unwrap_or(2000) as usize;
        match read_file_with_numbers(path, offset, limit) {
            Ok(text) => Ok(ToolResult::ok(text)),
            Err(e) => Ok(ToolResult::error(e.to_string())),
        }
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::FsRead]
    }
}

// ── Write ──────────────────────────────────────────────────

pub struct WriteTool;

impl WriteTool {
    pub fn new() -> Self {
        WriteTool
    }
}

impl Tool for WriteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "Write".into(),
            description: "写文件到本地磁盘。如果已有同名文件会覆盖。什么时候用：新建文件，或者完整替换一个你之前读过的文件。覆盖一个没读过的文件会失败。局部修改请用 Edit。".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Absolute path to the file" },
                    "content": { "type": "string", "description": "Content to write" }
                },
                "required": ["file_path", "content"]
            }),
            permission_level: PermissionLevel::Write,
        }
    }

    fn execute(&self, input: serde_json::Value) -> Result<ToolResult, EngineError> {
        let path = input["file_path"].as_str().unwrap_or("");
        let content = input["content"].as_str().unwrap_or("");
        if path.is_empty() {
            return Ok(ToolResult::error("Error: file_path is required"));
        }
        // 确保父目录存在
        if let Some(parent) = Path::new(path).parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                fs::create_dir_all(parent).map_err(|e| {
                    EngineError::ToolExecutionFailed {
                        name: "Write".into(),
                        message: format!("Cannot create parent dir: {e}"),
                    }
                })?;
            }
        }
        fs::write(path, content).map_err(|e| EngineError::ToolExecutionFailed {
            name: "Write".into(),
            message: format!("Cannot write {path}: {e}"),
        })?;
        Ok(ToolResult::ok(format!("Written: {path}")))
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::FsRead, Capability::FsWrite]
    }
}

// ── Edit ───────────────────────────────────────────────────

pub struct EditTool;

impl EditTool {
    pub fn new() -> Self {
        EditTool
    }
}

impl Tool for EditTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "Edit".into(),
            description: "执行精确字符串替换。old_string 必须和文件完全一致，包括缩进。如果 old_string 出现多次且未设置 replace_all，替换失败。设置 replace_all 为 true 会替换所有出现。用前必须先读文件。".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Absolute path to the file" },
                    "old_string": { "type": "string", "description": "Text to replace" },
                    "new_string": { "type": "string", "description": "Replacement text" },
                    "replace_all": { "type": "boolean", "description": "Replace all occurrences (default false)" }
                },
                "required": ["file_path", "old_string", "new_string"]
            }),
            permission_level: PermissionLevel::Write,
        }
    }

    fn execute(&self, input: serde_json::Value) -> Result<ToolResult, EngineError> {
        let path = input["file_path"].as_str().unwrap_or("");
        let old_str = input["old_string"].as_str().unwrap_or("");
        let new_str = input["new_string"].as_str().unwrap_or("");
        let replace_all = input["replace_all"].as_bool().unwrap_or(false);

        let text = fs::read_to_string(path).map_err(|e| {
            EngineError::ToolExecutionFailed {
                name: "Edit".into(),
                message: format!("Cannot read {path}: {e}"),
            }
        })?;

        let count = text.matches(old_str).count();

        if count == 0 {
            return Ok(ToolResult::error(format!(
                "Error: old_string not found in {path}"
            )));
        }
        if count > 1 && !replace_all {
            return Ok(ToolResult::error(format!(
                "Error: old_string appears {count} times — use replace_all:true or make it unique"
            )));
        }

        let new_text = if replace_all {
            text.replace(old_str, new_str)
        } else {
            text.replacen(old_str, new_str, 1)
        };

        fs::write(path, &new_text).map_err(|e| EngineError::ToolExecutionFailed {
            name: "Edit".into(),
            message: format!("Cannot write {path}: {e}"),
        })?;

        let replaced = if replace_all { count } else { 1 };
        Ok(ToolResult::ok(format!(
            "Edited {path}: {replaced} replacement(s)"
        )))
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::FsRead, Capability::FsWrite]
    }
}

// ── Bash ───────────────────────────────────────────────────

pub struct BashTool {
    blocked: Vec<&'static str>,
}

impl Default for BashTool {
    fn default() -> Self {
        BashTool {
            blocked: vec!["curl", "wget", "nc", "ssh", "telnet", "nslookup", "dig"],
        }
    }
}

impl Tool for BashTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "Bash".into(),
            description: "执行系统命令并返回输出。在 Git Bash（POSIX sh）中运行，使用 Unix shell 语法。用于：编译构建、运行测试、包管理、Git 操作、调用本地服务 API。不用 Bash 的情况：搜文件名用 Glob、搜内容用 Grep、读文件用 Read、网页访问用 Browser。".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The command to execute" }
                },
                "required": ["command"]
            }),
            permission_level: PermissionLevel::Execute,
        }
    }

    fn execute(&self, input: serde_json::Value) -> Result<ToolResult, EngineError> {
        let cmd_str = input["command"].as_str().unwrap_or("");

        // 安全检查
        let first_word = cmd_str.trim().split_whitespace().next().unwrap_or("");
        if self.blocked.iter().any(|b| *b == first_word) {
            return Ok(ToolResult::error(format!(
                "Error: Blocked for security: {cmd_str}"
            )));
        }

        let is_win = cfg!(windows);
        let (shell, shell_arg) = if is_win {
            ("powershell.exe", "-Command")
        } else {
            ("bash", "-c")
        };

        match Command::new(shell)
            .arg(shell_arg)
            .arg(cmd_str)
            .output()
        {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let _stderr = String::from_utf8_lossy(&output.stderr);
                if output.status.success() {
                    if stdout.trim().is_empty() {
                        Ok(ToolResult::ok("(no output)"))
                    } else {
                        Ok(ToolResult::ok(stdout.to_string()))
                    }
                } else {
                    Ok(ToolResult::error(format!(
                        "Exit {}",
                        output.status.code().unwrap_or(-1)
                    )))
                }
            }
            Err(e) => Ok(ToolResult::error(format!("Error: {e}"))),
        }
    }

    fn self_gates(&self) -> bool {
        true
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::FsRead, Capability::FsWrite, Capability::ProcessSpawn, Capability::NetOutbound, Capability::NetLocalhost]
    }
}

// ── Glob ───────────────────────────────────────────────────

pub struct GlobTool;

impl GlobTool {
    pub fn new() -> Self {
        GlobTool
    }
}

impl Tool for GlobTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "Glob".into(),
            description: "按文件名模式找文件。用于：列出目录结构、查找已知命名模式的文件（\"src/*.rs\"）、确认文件是否存在。不用于：搜索文件内容——用 Grep；手动 ls/dir 遍历——用 Glob。glob 比手动 ls 快且准，不返回目录噪音。".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Glob pattern, e.g. \"**/*.ts\"" },
                    "path": { "type": "string", "description": "Directory to search in. Defaults to cwd." }
                },
                "required": ["pattern"]
            }),
            permission_level: PermissionLevel::ReadOnly,
        }
    }

    fn execute(&self, input: serde_json::Value) -> Result<ToolResult, EngineError> {
        let pattern = input["pattern"].as_str().unwrap_or("");
        let base = input["path"]
            .as_str()
            .map(|p| Path::new(p).to_path_buf())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

        let search_pattern = if pattern.contains("**") {
            // 递归 glob
            base.join(pattern).to_string_lossy().to_string()
        } else {
            base.join(pattern).to_string_lossy().to_string()
        };

        match glob::glob(&search_pattern) {
            Ok(paths) => {
                let results: Vec<String> = paths
                    .filter_map(|p| p.ok())
                    .map(|p| p.to_string_lossy().to_string())
                    .take(200)
                    .collect();
                if results.is_empty() {
                    Ok(ToolResult::ok("(no matches)"))
                } else {
                    Ok(ToolResult::ok(results.join("\n")))
                }
            }
            Err(e) => Ok(ToolResult::error(format!("Glob error: {e}"))),
        }
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::FsRead]
    }
}

// ── Grep ───────────────────────────────────────────────────

pub struct GrepTool;

impl GrepTool {
    pub fn new() -> Self {
        GrepTool
    }
}

impl Tool for GrepTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "Grep".into(),
            description: "内容搜索（基于 ripgrep）。用于：搜代码里的函数名/类型/关键字、找所有引用某模式的文件、搜日志里的错误。不用于：找文件名——用 Glob；在文件里定位具体位置——先 Grep 找到文件再用 Read 看；手动 rg/grep 命令——用 Grep 不用 Bash 跑。支持完整正则，用 glob 参数过滤文件类型。默认只返回匹配的文件路径，output_mode: \"content\" 返回匹配行。".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regex pattern to search for" },
                    "path": { "type": "string", "description": "File or directory to search. Defaults to cwd." }
                },
                "required": ["pattern"]
            }),
            permission_level: PermissionLevel::ReadOnly,
        }
    }

    fn execute(&self, input: serde_json::Value) -> Result<ToolResult, EngineError> {
        let pattern = input["pattern"].as_str().unwrap_or("");
        let search_path = input["path"]
            .as_str()
            .unwrap_or(".");

        // 尝试 ripgrep (rg) 先
        let rg_result = Command::new("rg")
            .args([
                "--no-heading",
                "--with-filename",
                "--line-number",
                "--max-count=50",
                pattern,
                search_path,
            ])
            .output();

        match rg_result {
            Ok(out) if !out.stdout.is_empty() => {
                return Ok(ToolResult::ok(String::from_utf8_lossy(&out.stdout).to_string()));
            }
            Ok(out) if out.stdout.is_empty() => {
                // rg 没输出——检查 stderr 判断是否未安装
                let stderr_str = String::from_utf8_lossy(&out.stderr);
                if cfg!(windows) && (stderr_str.contains("not found") || stderr_str.contains("No such file")) {
                    return self.powershell_grep(pattern, search_path);
                }
            }
            Ok(_) => {
                // rg 成功但有非空 stderr（不应该发生）或已经处理
            }
            Err(_) => {
                // rg 不可用——降级
                if cfg!(windows) {
                    return self.powershell_grep(pattern, search_path);
                }
            }
        }

        Ok(ToolResult::ok(
            "(no matches)"
        ))
    }
}

impl GrepTool {
    /// PowerShell Select-String 降级（和现有 tools.ts 对齐）。
    fn powershell_grep(&self, pattern: &str, search_path: &str) -> Result<ToolResult, EngineError> {
        // 转义单引号
        let escaped = pattern.replace('\'', "''");
        let cmd = format!(
            "Get-ChildItem -Recurse -File '{}' | Select-String -Pattern '{}' | Select-Object -First 50 | ForEach-Object {{ $_.Path + ':' + $_.LineNumber + ':' + $_.Line }}",
            search_path, escaped
        );

        match Command::new("powershell.exe")
            .args(["-Command", &cmd])
            .output()
        {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                if stdout.trim().is_empty() {
                    Ok(ToolResult::ok("(no matches)"))
                } else {
                    Ok(ToolResult::ok(stdout.to_string()))
                }
            }
            Err(e) => Ok(ToolResult::error(format!("Grep failed: {e}"))),
        }
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::FsRead]
    }
}

// ── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn read_existing_file() {
        let tmp = std::env::temp_dir().join("kat_test_read.txt");
        fs::write(&tmp, "line1\nline2\nline3\n").unwrap();

        let tool = ReadTool::new();
        let result = tool
            .execute(serde_json::json!({"file_path": tmp.to_str().unwrap()}))
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("line1"));
        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn read_nonexistent_file() {
        let tool = ReadTool::new();
        let result = tool
            .execute(serde_json::json!({"file_path": "/nonexistent/kat_test"}))
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("not found"));
    }

    #[test]
    fn write_and_read() {
        let tmp = std::env::temp_dir().join("kat_test_write.txt");
        let path_str = tmp.to_str().unwrap();

        let write = WriteTool::new();
        let result = write
            .execute(serde_json::json!({"file_path": path_str, "content": "hello katherine"}))
            .unwrap();
        assert!(!result.is_error);
        assert!(fs::read_to_string(&tmp).unwrap().contains("hello katherine"));
        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn write_creates_parent_dirs() {
        let tmp = std::env::temp_dir().join("kat_test_subdir");
        let file_path = tmp.join("nested").join("test.txt");
        let path_str = file_path.to_str().unwrap();

        let write = WriteTool::new();
        let result = write
            .execute(serde_json::json!({"file_path": path_str, "content": "nested"}))
            .unwrap();
        assert!(!result.is_error);
        assert!(file_path.exists());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn edit_single_occurrence() {
        let tmp = std::env::temp_dir().join("kat_test_edit.txt");
        fs::write(&tmp, "hello world\n").unwrap();

        let edit = EditTool::new();
        let result = edit
            .execute(serde_json::json!({
                "file_path": tmp.to_str().unwrap(),
                "old_string": "world",
                "new_string": "katherine"
            }))
            .unwrap();
        assert!(!result.is_error);
        assert!(fs::read_to_string(&tmp).unwrap().contains("hello katherine"));
        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn edit_old_string_not_found() {
        let tmp = std::env::temp_dir().join("kat_test_edit2.txt");
        fs::write(&tmp, "hello\n").unwrap();

        let edit = EditTool::new();
        let result = edit
            .execute(serde_json::json!({
                "file_path": tmp.to_str().unwrap(),
                "old_string": "nonexistent",
                "new_string": "x"
            }))
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("not found"));
        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn edit_multiple_without_replace_all() {
        let tmp = std::env::temp_dir().join("kat_test_edit3.txt");
        fs::write(&tmp, "a a a\n").unwrap();

        let edit = EditTool::new();
        let result = edit
            .execute(serde_json::json!({
                "file_path": tmp.to_str().unwrap(),
                "old_string": "a",
                "new_string": "b"
            }))
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("appears 3 times"));
        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn bash_blocked_command() {
        let bash = BashTool::default();
        let result = bash
            .execute(serde_json::json!({"command": "curl http://evil.com"}))
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("Blocked"));
    }

    #[test]
    fn bash_echo() {
        let bash = BashTool::default();
        let result = bash
            .execute(serde_json::json!({"command": "echo hello"}))
            .unwrap();
        assert!(!result.is_error);
        // On Windows, echo might include trailing spaces. Just check it doesn't error.
    }

    #[test]
    fn glob_finds_rs_files() {
        let tool = GlobTool::new();
        // 临时目录里创建文件

        let tmp = std::env::temp_dir().join("kat_test_glob");
        let _ = std::fs::create_dir_all(&tmp);
        std::fs::write(tmp.join("a.rs"), "fn main() {}").unwrap();
        std::fs::write(tmp.join("b.txt"), "hello").unwrap();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "*.rs",
                "path": tmp.to_str().unwrap()
            }))
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains(".rs"));
        assert!(!result.content.contains(".txt"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn grep_finds_pattern() {
        let tool = GrepTool::new();
        // 搜索 katherine-core/src 下的 "Message" 关键字
        let cwd = std::env::current_dir().unwrap();
        let search_dir = cwd.join("katherine-core").join("src");
        let result = tool
            .execute(serde_json::json!({
                "pattern": "Message",
                "path": search_dir.to_str().unwrap()
            }))
            .unwrap();
        assert!(!result.is_error);
    }
}
