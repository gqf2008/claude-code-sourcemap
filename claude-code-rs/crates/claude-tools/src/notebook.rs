//! NotebookEditTool — edit Jupyter notebook (.ipynb) cells.
//!
//! Aligned with TS `NotebookEditTool.ts`.  Supports three edit modes:
//! - replace: replace an existing cell's source
//! - insert: insert a new cell at a position
//! - delete: remove a cell

use async_trait::async_trait;
use claude_core::tool::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};

use crate::path_util;

pub struct NotebookEditTool;

#[async_trait]
impl Tool for NotebookEditTool {
    fn name(&self) -> &str { "NotebookEdit" }

    fn description(&self) -> &str {
        "Edit Jupyter notebook (.ipynb) cells. Supports replacing cell content, \
         inserting new cells, and deleting cells. Always read the notebook first \
         to understand its structure before editing."
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
            Err(e) => return Ok(ToolResult::error(format!("{}", e))),
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
            .map_err(|e| anyhow::anyhow!("Failed to read notebook: {}", e))?;

        let mut notebook: Value = serde_json::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Invalid notebook JSON: {}", e))?;

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
                    .map(|l| Value::String(format!("{}\n", l)))
                    .collect();
                cells[cell_number]["source"] = Value::Array(source_lines);
                cells[cell_number]["execution_count"] = Value::Null;
                cells[cell_number]["outputs"] = json!([]);
            }
            "insert" => {
                let source_lines: Vec<Value> = new_source
                    .lines()
                    .map(|l| Value::String(format!("{}\n", l)))
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
                    "Invalid edit_mode: {}. Use replace, insert, or delete.", edit_mode
                )));
            }
        }

        let updated = serde_json::to_string_pretty(&notebook)?;
        std::fs::write(&path, &updated)?;

        Ok(ToolResult::text(format!(
            "Notebook {} updated: {} cell at index {}. Total cells: {}",
            path.display(), edit_mode, cell_number,
            notebook["cells"].as_array().map(|c| c.len()).unwrap_or(0)
        )))
    }
}
