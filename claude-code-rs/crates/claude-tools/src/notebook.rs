//! `NotebookEditTool` — edit Jupyter notebook (.ipynb) cells.
//!
//! Aligned with TS `NotebookEditTool.ts`.  Supports three edit modes:
//! - replace: replace an existing cell's source
//! - insert: insert a new cell at a position
//! - delete: remove a cell

use async_trait::async_trait;
use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};

use crate::path_util;

pub struct NotebookEditTool;

#[async_trait]
impl Tool for NotebookEditTool {
    fn name(&self) -> &'static str { "NotebookEdit" }
    fn category(&self) -> ToolCategory { ToolCategory::FileSystem }

    fn description(&self) -> &'static str {
        "Edit Jupyter notebook (.ipynb) cells. Supports replacing cell content, \
         inserting new cells, and deleting cells. Always read the notebook first \
         to understand its structure before editing."
    }

    fn to_auto_classifier_input(&self, input: &Value) -> Value {
        let path = input.get("notebook_path").cloned().unwrap_or(Value::Null);
        let mode = input.get("edit_mode").cloned().unwrap_or(Value::Null);
        let cell = input.get("cell_number").cloned().unwrap_or(Value::Null);
        json!({"NotebookEdit": {"path": path, "mode": mode, "cell": cell}})
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "notebook_path": {
                    "type": "string",
                    "description": "Absolute path to the .ipynb file"
                },
                "cell_number": {
                    "type": "integer",
                    "description": "0-based index of the cell to edit. For insert, the new cell is placed after this index."
                },
                "new_source": {
                    "type": "string",
                    "description": "New source code or markdown for the cell"
                },
                "cell_type": {
                    "type": "string",
                    "enum": ["code", "markdown"],
                    "description": "Cell type (required for insert mode)"
                },
                "edit_mode": {
                    "type": "string",
                    "enum": ["replace", "insert", "delete"],
                    "description": "Operation: replace (default), insert, or delete"
                }
            },
            "required": ["notebook_path", "new_source"]
        })
    }

    fn is_read_only(&self) -> bool { false }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let notebook_path = input["notebook_path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing notebook_path"))?;

        if !notebook_path.ends_with(".ipynb") {
            return Ok(ToolResult::error("File must be a .ipynb notebook"));
        }

        let path = match path_util::resolve_path(notebook_path, &context.cwd) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(format!("{e}"))),
        };

        let new_source = input["new_source"]
            .as_str()
            .unwrap_or("");
        let cell_number = input["cell_number"]
            .as_u64()
            .unwrap_or(0) as usize;
        let cell_type = input["cell_type"]
            .as_str()
            .unwrap_or("code");
        let edit_mode = input["edit_mode"]
            .as_str()
            .unwrap_or("replace");

        let content = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("Failed to read notebook: {e}"))?;

        let mut notebook: Value = serde_json::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Invalid notebook JSON: {e}"))?;

        let cells = notebook["cells"]
            .as_array_mut()
            .ok_or_else(|| anyhow::anyhow!("Notebook has no cells array"))?;

        match edit_mode {
            "replace" => {
                if cell_number >= cells.len() {
                    return Ok(ToolResult::error(format!(
                        "Cell index {} out of range (notebook has {} cells)",
                        cell_number, cells.len()
                    )));
                }
                let source_lines: Vec<Value> = new_source
                    .lines()
                    .map(|l| Value::String(format!("{l}\n")))
                    .collect();
                cells[cell_number]["source"] = Value::Array(source_lines);
                cells[cell_number]["execution_count"] = Value::Null;
                cells[cell_number]["outputs"] = json!([]);
            }
            "insert" => {
                let source_lines: Vec<Value> = new_source
                    .lines()
                    .map(|l| Value::String(format!("{l}\n")))
                    .collect();
                let new_cell = json!({
                    "cell_type": cell_type,
                    "metadata": {},
                    "source": source_lines,
                    "outputs": [],
                    "execution_count": null
                });
                let insert_at = (cell_number + 1).min(cells.len());
                cells.insert(insert_at, new_cell);
            }
            "delete" => {
                if cell_number >= cells.len() {
                    return Ok(ToolResult::error(format!(
                        "Cell index {} out of range (notebook has {} cells)",
                        cell_number, cells.len()
                    )));
                }
                cells.remove(cell_number);
            }
            _ => {
                return Ok(ToolResult::error(format!(
                    "Invalid edit_mode: {edit_mode}. Use replace, insert, or delete."
                )));
            }
        }

        let updated = serde_json::to_string_pretty(&notebook)?;
        std::fs::write(&path, &updated)?;

        Ok(ToolResult::text(format!(
            "Notebook {} updated: {} cell at index {}. Total cells: {}",
            path.display(), edit_mode, cell_number,
            notebook["cells"].as_array().map_or(0, std::vec::Vec::len)
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_core::tool::AbortSignal;
    use claude_core::permissions::PermissionMode;

    fn ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext {
            cwd: dir.to_path_buf(),
            abort_signal: AbortSignal::new(),
            permission_mode: PermissionMode::Default,
            messages: vec![],
        }
    }

    fn result_text(r: &ToolResult) -> String {
        match &r.content[0] {
            claude_core::message::ToolResultContent::Text { text } => text.clone(),
            _ => String::new(),
        }
    }

    fn sample_notebook() -> String {
        json!({
            "nbformat": 4,
            "nbformat_minor": 5,
            "metadata": {},
            "cells": [
                {
                    "cell_type": "code",
                    "metadata": {},
                    "source": ["print('hello')\n"],
                    "outputs": [],
                    "execution_count": 1
                },
                {
                    "cell_type": "markdown",
                    "metadata": {},
                    "source": ["# Title\n"]
                }
            ]
        }).to_string()
    }

    #[tokio::test]
    async fn replace_cell() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nb_path = tmp.path().join("test.ipynb");
        std::fs::write(&nb_path, sample_notebook()).unwrap();

        let tool = NotebookEditTool;
        let input = json!({
            "notebook_path": nb_path.to_str().unwrap(),
            "cell_number": 0,
            "new_source": "print('updated')",
            "edit_mode": "replace"
        });
        let result = tool.call(input, &ctx(tmp.path())).await.unwrap();
        assert!(!result.is_error);
        assert!(result_text(&result).contains("replace"));

        let updated: Value = serde_json::from_str(&std::fs::read_to_string(&nb_path).unwrap()).unwrap();
        let source = updated["cells"][0]["source"][0].as_str().unwrap();
        assert!(source.contains("updated"));
    }

    #[tokio::test]
    async fn insert_cell() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nb_path = tmp.path().join("test.ipynb");
        std::fs::write(&nb_path, sample_notebook()).unwrap();

        let tool = NotebookEditTool;
        let input = json!({
            "notebook_path": nb_path.to_str().unwrap(),
            "cell_number": 0,
            "new_source": "x = 42",
            "cell_type": "code",
            "edit_mode": "insert"
        });
        let result = tool.call(input, &ctx(tmp.path())).await.unwrap();
        assert!(!result.is_error);

        let updated: Value = serde_json::from_str(&std::fs::read_to_string(&nb_path).unwrap()).unwrap();
        assert_eq!(updated["cells"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn delete_cell() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nb_path = tmp.path().join("test.ipynb");
        std::fs::write(&nb_path, sample_notebook()).unwrap();

        let tool = NotebookEditTool;
        let input = json!({
            "notebook_path": nb_path.to_str().unwrap(),
            "cell_number": 1,
            "new_source": "",
            "edit_mode": "delete"
        });
        let result = tool.call(input, &ctx(tmp.path())).await.unwrap();
        assert!(!result.is_error);

        let updated: Value = serde_json::from_str(&std::fs::read_to_string(&nb_path).unwrap()).unwrap();
        assert_eq!(updated["cells"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn replace_out_of_range() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nb_path = tmp.path().join("test.ipynb");
        std::fs::write(&nb_path, sample_notebook()).unwrap();

        let tool = NotebookEditTool;
        let input = json!({
            "notebook_path": nb_path.to_str().unwrap(),
            "cell_number": 99,
            "new_source": "x",
            "edit_mode": "replace"
        });
        let result = tool.call(input, &ctx(tmp.path())).await.unwrap();
        assert!(result.is_error);
        assert!(result_text(&result).contains("out of range"));
    }

    #[tokio::test]
    async fn rejects_non_ipynb() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = NotebookEditTool;
        let input = json!({
            "notebook_path": "test.py",
            "new_source": "x = 1"
        });
        let result = tool.call(input, &ctx(tmp.path())).await.unwrap();
        assert!(result.is_error);
        assert!(result_text(&result).contains(".ipynb"));
    }
}
