//! `WorkflowTool` — execute reusable workflow scripts from YAML or JSON files.
//!
//! Aligned with TS `WorkflowTool` (WORKFLOW_SCRIPTS feature gate).
//!
//! A workflow file defines a named sequence of steps, each of which can be:
//! - `bash` — run a shell command and capture output
//! - `read` — read a file and inject its content
//! - `write` — write content to a file
//! - `message` — inject a text message (e.g., instructions for the next step)
//! - `glob` — list files matching a pattern
//!
//! Template variables (`{{VAR}}`) in string fields are substituted from the
//! `variables` input or from environment variables.
//!
//! # Workflow file format (YAML)
//!
//! ```yaml
//! name: "My Workflow"
//! description: "Optional description"
//! steps:
//!   - name: "Build"
//!     type: "bash"
//!     command: "cargo build --release"
//!   - name: "Read manifest"
//!     type: "read"
//!     path: "Cargo.toml"
//!   - name: "Note"
//!     type: "message"
//!     content: "Build complete. Check the output above."
//! ```

use async_trait::async_trait;
use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct WorkflowTool;

/// A single step in a workflow.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowStep {
    /// Human-readable step name.
    #[serde(default)]
    name: String,
    /// Step type: bash, read, write, message, glob.
    #[serde(rename = "type")]
    step_type: String,
    // bash
    #[serde(default)]
    command: String,
    // read / write / glob
    #[serde(default)]
    path: String,
    // write
    #[serde(default)]
    content: String,
    // message
    // (reuses `content` field)
    // glob
    #[serde(default)]
    pattern: String,
    /// If true, skip this step without error.
    #[serde(default)]
    optional: bool,
}

/// Workflow definition loaded from a file.
#[derive(Debug, Deserialize)]
struct WorkflowDef {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    steps: Vec<WorkflowStep>,
}

#[async_trait]
impl Tool for WorkflowTool {
    fn name(&self) -> &'static str { "Workflow" }
    fn category(&self) -> ToolCategory { ToolCategory::Session }

    fn description(&self) -> &'static str {
        "Execute a reusable workflow script from a YAML or JSON file. \
         Workflows define named sequences of steps: bash commands, file reads/writes, \
         messages, and glob patterns. Template variables ({{VAR}}) are substituted."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["workflowPath"],
            "properties": {
                "workflowPath": {
                    "type": "string",
                    "description": "Path to the workflow YAML or JSON file."
                },
                "variables": {
                    "type": "object",
                    "description": "Template variables to substitute for {{VAR}} placeholders.",
                    "additionalProperties": { "type": "string" }
                },
                "dryRun": {
                    "type": "boolean",
                    "description": "If true, describe steps without executing them (default: false).",
                    "default": false
                }
            }
        })
    }

    fn is_read_only(&self) -> bool { false }

    async fn call(&self, input: Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
        let cwd = &ctx.cwd;

        let workflow_path = input["workflowPath"].as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'workflowPath'"))?;
        let dry_run = input["dryRun"].as_bool().unwrap_or(false);

        // Build variables map from input + env
        let mut vars: HashMap<String, String> = std::env::vars().collect();
        if let Some(obj) = input["variables"].as_object() {
            for (k, v) in obj {
                if let Some(s) = v.as_str() {
                    vars.insert(k.clone(), s.to_string());
                }
            }
        }

        let abs_path = resolve_path(cwd, workflow_path);
        if !abs_path.exists() {
            return Ok(ToolResult::error(format!(
                "Workflow file not found: {}", abs_path.display()
            )));
        }

        let content = tokio::fs::read_to_string(&abs_path).await
            .map_err(|e| anyhow::anyhow!("Cannot read workflow file: {e}"))?;

        let workflow = parse_workflow(&content, &abs_path)?;

        let header = format!(
            "# Workflow: {}\n{}\n({} steps)",
            if workflow.name.is_empty() { workflow_path } else { &workflow.name },
            if workflow.description.is_empty() { String::new() } else { format!("{}\n", workflow.description) },
            workflow.steps.len()
        );

        if dry_run {
            let steps: Vec<String> = workflow.steps.iter().enumerate().map(|(i, s)| {
                let kind = &s.step_type;
                let detail = match kind.as_str() {
                    "bash" => format!("$ {}", s.command),
                    "read" => format!("read {}", s.path),
                    "write" => format!("write {} ({} bytes)", s.path, s.content.len()),
                    "message" => format!("message: {}", truncate(&s.content, 80)),
                    "glob" => format!("glob {}", s.pattern),
                    _ => format!("unknown step type '{kind}'"),
                };
                format!("  {}. [{}] {} — {}", i + 1, kind, s.name, detail)
            }).collect();
            return Ok(ToolResult::text(format!("{}\n\nDry run — steps:\n{}", header, steps.join("\n"))));
        }

        // Execute steps
        let mut results = vec![header];
        let workflow_dir = abs_path.parent().unwrap_or(cwd);

        for (i, step) in workflow.steps.iter().enumerate() {
            let step_label = if step.name.is_empty() {
                format!("Step {}", i + 1)
            } else {
                format!("Step {} ({})", i + 1, step.name)
            };

            results.push(format!("\n## {}", step_label));

            let result = execute_step(step, cwd, workflow_dir, &vars).await;
            match result {
                Ok(output) => {
                    if !output.is_empty() {
                        results.push(output);
                    } else {
                        results.push("(no output)".to_string());
                    }
                }
                Err(e) => {
                    if step.optional {
                        results.push(format!("⚠ Skipped (optional): {e}"));
                    } else {
                        results.push(format!("✗ Failed: {e}"));
                        return Ok(ToolResult::text(results.join("\n")));
                    }
                }
            }
        }

        results.push(format!("\n✓ Workflow complete ({} steps).", workflow.steps.len()));
        Ok(ToolResult::text(results.join("\n")))
    }
}

/// Parse a workflow file (YAML or JSON, detected by extension).
fn parse_workflow(content: &str, path: &Path) -> anyhow::Result<WorkflowDef> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("yaml");
    match ext {
        "json" => {
            serde_json::from_str(content)
                .map_err(|e| anyhow::anyhow!("Invalid JSON workflow: {e}"))
        }
        _ => {
            serde_yaml::from_str(content)
                .map_err(|e| anyhow::anyhow!("Invalid YAML workflow: {e}"))
        }
    }
}

/// Execute a single workflow step, returning its output text.
async fn execute_step(
    step: &WorkflowStep,
    cwd: &Path,
    workflow_dir: &Path,
    vars: &HashMap<String, String>,
) -> anyhow::Result<String> {
    match step.step_type.as_str() {
        "bash" | "shell" => {
            let cmd = interpolate(&step.command, vars);
            if cmd.is_empty() {
                anyhow::bail!("'bash' step requires 'command'");
            }
            let output = execute_bash(&cmd, cwd).await?;
            Ok(format!("```\n$ {cmd}\n{output}\n```"))
        }

        "read" | "file_read" => {
            let p = interpolate(&step.path, vars);
            if p.is_empty() {
                anyhow::bail!("'read' step requires 'path'");
            }
            let abs = resolve_path(workflow_dir, &p);
            let content = tokio::fs::read_to_string(&abs).await
                .map_err(|e| anyhow::anyhow!("Cannot read {}: {e}", abs.display()))?;
            Ok(format!("**{}**\n```\n{}\n```", p, content.trim()))
        }

        "write" | "file_write" => {
            let p = interpolate(&step.path, vars);
            let body = interpolate(&step.content, vars);
            if p.is_empty() {
                anyhow::bail!("'write' step requires 'path'");
            }
            let abs = resolve_path(workflow_dir, &p);
            if let Some(parent) = abs.parent() {
                tokio::fs::create_dir_all(parent).await.ok();
            }
            tokio::fs::write(&abs, body.as_bytes()).await
                .map_err(|e| anyhow::anyhow!("Cannot write {}: {e}", abs.display()))?;
            Ok(format!("Wrote {} ({} bytes)", p, body.len()))
        }

        "message" | "note" => {
            let msg = interpolate(&step.content, vars);
            Ok(msg)
        }

        "glob" | "list" => {
            let pat = interpolate(&step.pattern, vars);
            if pat.is_empty() {
                anyhow::bail!("'glob' step requires 'pattern'");
            }
            let files = glob_files(cwd, &pat)?;
            if files.is_empty() {
                Ok(format!("No files matching `{pat}`."))
            } else {
                Ok(format!("Files matching `{pat}`:\n{}", files.join("\n")))
            }
        }

        other => anyhow::bail!("Unknown step type: '{other}'"),
    }
}

/// Substitute `{{VAR}}` placeholders with values from `vars`.
fn interpolate(template: &str, vars: &HashMap<String, String>) -> String {
    let mut result = template.to_string();
    for (k, v) in vars {
        result = result.replace(&format!("{{{{{k}}}}}"), v);
    }
    result
}

/// Run a bash command and capture combined stdout+stderr.
async fn execute_bash(cmd: &str, cwd: &Path) -> anyhow::Result<String> {
    use tokio::process::Command;
    use std::time::Duration;

    #[cfg(windows)]
    let child = {
        Command::new("cmd")
            .args(["/C", cmd])
            .current_dir(cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?
    };
    #[cfg(not(windows))]
    let child = {
        Command::new("sh")
            .args(["-c", cmd])
            .current_dir(cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?
    };

    let result = tokio::time::timeout(Duration::from_secs(60), child.wait_with_output()).await
        .map_err(|_| anyhow::anyhow!("Command timed out after 60s"))??;

    let stdout = String::from_utf8_lossy(&result.stdout).to_string();
    let stderr = String::from_utf8_lossy(&result.stderr).to_string();

    let mut out = String::new();
    if !stdout.trim().is_empty() { out.push_str(stdout.trim()); }
    if !stderr.trim().is_empty() {
        if !out.is_empty() { out.push('\n'); }
        out.push_str(stderr.trim());
    }

    if !result.status.success() {
        let code = result.status.code().unwrap_or(-1);
        if out.is_empty() {
            anyhow::bail!("Command exited with code {code}");
        }
        // Return output even on failure (it may contain useful error info)
        return Ok(format!("{out}\n[exit code: {code}]"));
    }

    Ok(out)
}

/// Simple glob using walkdir + pattern matching.
fn glob_files(cwd: &Path, pattern: &str) -> anyhow::Result<Vec<String>> {
    use std::time::Instant;

    // Use glob crate if available, otherwise walk dir
    let glob_pattern = if std::path::Path::new(pattern).is_absolute() {
        pattern.to_string()
    } else {
        cwd.join(pattern).to_string_lossy().to_string()
    };

    let mut files = Vec::new();
    let deadline = Instant::now() + std::time::Duration::from_secs(5);

    match glob::glob(&glob_pattern) {
        Ok(paths) => {
            for entry in paths.flatten() {
                if Instant::now() > deadline { break; }
                let rel = entry.strip_prefix(cwd).unwrap_or(&entry);
                files.push(rel.to_string_lossy().to_string());
                if files.len() >= 100 { break; }
            }
        }
        Err(e) => anyhow::bail!("Invalid glob pattern: {e}"),
    }

    files.sort();
    Ok(files)
}

fn resolve_path(cwd: &Path, p: &str) -> PathBuf {
    let path = Path::new(p);
    if path.is_absolute() { path.to_path_buf() } else { cwd.join(path) }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max { s } else { &s[..max] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interpolate_basic() {
        let mut vars = HashMap::new();
        vars.insert("NAME".to_string(), "world".to_string());
        assert_eq!(interpolate("Hello, {{NAME}}!", &vars), "Hello, world!");
    }

    #[test]
    fn test_interpolate_multiple() {
        let mut vars = HashMap::new();
        vars.insert("A".to_string(), "foo".to_string());
        vars.insert("B".to_string(), "bar".to_string());
        assert_eq!(interpolate("{{A}}-{{B}}", &vars), "foo-bar");
    }

    #[test]
    fn test_interpolate_no_match() {
        let vars = HashMap::new();
        assert_eq!(interpolate("{{MISSING}}", &vars), "{{MISSING}}");
    }

    #[test]
    fn test_parse_workflow_json() {
        let json = r#"{"name":"test","steps":[{"name":"s1","type":"message","content":"hi"}]}"#;
        let wf = parse_workflow(json, Path::new("wf.json")).unwrap();
        assert_eq!(wf.name, "test");
        assert_eq!(wf.steps.len(), 1);
        assert_eq!(wf.steps[0].step_type, "message");
    }

    #[test]
    fn test_parse_workflow_yaml() {
        let yaml = "name: my-wf\nsteps:\n  - name: step1\n    type: bash\n    command: echo hi\n";
        let wf = parse_workflow(yaml, Path::new("wf.yaml")).unwrap();
        assert_eq!(wf.name, "my-wf");
        assert_eq!(wf.steps[0].command, "echo hi");
    }

    #[test]
    fn test_parse_workflow_empty_steps() {
        let yaml = "name: empty\n";
        let wf = parse_workflow(yaml, Path::new("wf.yaml")).unwrap();
        assert!(wf.steps.is_empty());
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello world", 5), "hello");
        assert_eq!(truncate("hi", 10), "hi");
    }

    #[test]
    fn test_resolve_path_absolute() {
        let cwd = Path::new("/home/user");
        let abs = if cfg!(windows) { "C:\\tmp\\file" } else { "/tmp/file" };
        assert_eq!(resolve_path(cwd, abs), PathBuf::from(abs));
    }

    #[test]
    fn test_resolve_path_relative() {
        let cwd = Path::new("/home/user");
        assert_eq!(resolve_path(cwd, "src/main.rs"), PathBuf::from("/home/user/src/main.rs"));
    }
}
