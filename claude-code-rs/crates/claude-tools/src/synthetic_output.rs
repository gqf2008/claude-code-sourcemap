use async_trait::async_trait;
use claude_core::tool::{Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};

/// `SyntheticOutputTool` — return structured output in a requested format.
///
/// Used in `--print` / non-interactive mode to let the model produce validated
/// JSON output conforming to a caller-supplied schema.  The model MUST call
/// this tool exactly once at the end of its response.
///
/// When constructed via [`SyntheticOutputTool::with_schema`], the tool
/// validates the input against the given JSON Schema at call-time.
///
/// Mirrors the TS `SyntheticOutputTool` from `tools/SyntheticOutputTool/`.
pub struct SyntheticOutputTool {
    /// Optional JSON Schema to validate against.  If `None`, any object is
    /// accepted (pass-through mode).
    schema: Option<Value>,
}

impl SyntheticOutputTool {
    /// Create a pass-through output tool (no schema validation).
    pub fn new() -> Self {
        Self { schema: None }
    }

    /// Create an output tool that validates against the given JSON Schema.
    pub fn with_schema(schema: Value) -> Self {
        Self { schema: Some(schema) }
    }

    /// Basic structural validation against schema.
    ///
    /// This is a simplified validator that checks:
    /// - required properties are present
    /// - property types match (string, number, boolean, array, object)
    ///
    /// For production use, a full JSON Schema validator (e.g. `jsonschema` crate)
    /// could replace this.
    fn validate(&self, input: &Value) -> Result<(), String> {
        let schema = match &self.schema {
            Some(s) => s,
            None => return Ok(()),
        };

        // Check required fields
        if let Some(required) = schema["required"].as_array() {
            for req in required {
                if let Some(key) = req.as_str() {
                    if input.get(key).is_none() || input[key].is_null() {
                        return Err(format!("Missing required field: '{key}'"));
                    }
                }
            }
        }

        // Check property types
        if let Some(props) = schema["properties"].as_object() {
            for (key, prop_schema) in props {
                if let Some(value) = input.get(key) {
                    if !value.is_null() {
                        if let Some(expected_type) = prop_schema["type"].as_str() {
                            let ok = match expected_type {
                                "string" => value.is_string(),
                                "number" | "integer" => value.is_number(),
                                "boolean" => value.is_boolean(),
                                "array" => value.is_array(),
                                "object" => value.is_object(),
                                "null" => value.is_null(),
                                _ => true,
                            };
                            if !ok {
                                return Err(format!(
                                    "Field '{key}' expected type '{expected_type}', got {}",
                                    value_type_name(value)
                                ));
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

impl Default for SyntheticOutputTool {
    fn default() -> Self { Self::new() }
}

fn value_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[async_trait]
impl Tool for SyntheticOutputTool {
    fn name(&self) -> &'static str { "SyntheticOutput" }
    fn category(&self) -> ToolCategory { ToolCategory::Session }

    fn description(&self) -> &'static str {
        "Return structured output in the requested format. \
         Use this tool to return your final response in the requested structured format. \
         You MUST call this tool exactly once at the end of your response to provide \
         the structured output."
    }

    fn input_schema(&self) -> Value {
        if let Some(ref schema) = self.schema {
            schema.clone()
        } else {
            json!({
                "type": "object",
                "additionalProperties": true,
                "description": "Structured output matching the requested format."
            })
        }
    }

    fn is_read_only(&self) -> bool { true }
    fn is_concurrency_safe(&self) -> bool { true }

    async fn call(&self, input: Value, _context: &ToolContext) -> anyhow::Result<ToolResult> {
        // Validate against schema if provided
        if let Err(e) = self.validate(&input) {
            return Ok(ToolResult::error(format!(
                "Structured output validation failed: {e}. \
                 Please fix the output to match the requested schema and try again."
            )));
        }

        tracing::debug!(output = %input, "Structured output captured");

        // Return both human-readable confirmation and the structured data
        let mut result = ToolResult::text("Structured output provided successfully.");
        result.structured_output = Some(input);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_context() -> ToolContext {
        ToolContext::default()
    }

    #[test]
    fn passthrough_accepts_any_object() {
        let tool = SyntheticOutputTool::new();
        let input = json!({"foo": "bar", "num": 42});
        assert!(tool.validate(&input).is_ok());
    }

    #[test]
    fn validates_required_fields() {
        let tool = SyntheticOutputTool::with_schema(json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "age": { "type": "number" }
            },
            "required": ["name", "age"]
        }));

        assert!(tool.validate(&json!({"name": "Alice", "age": 30})).is_ok());
        assert!(tool.validate(&json!({"name": "Alice"})).is_err());
        assert!(tool.validate(&json!({"age": 30})).is_err());
    }

    #[test]
    fn validates_property_types() {
        let tool = SyntheticOutputTool::with_schema(json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "count": { "type": "integer" },
                "active": { "type": "boolean" }
            }
        }));

        assert!(tool.validate(&json!({"name": "X", "count": 5, "active": true})).is_ok());
        assert!(tool.validate(&json!({"name": 123})).is_err());
        assert!(tool.validate(&json!({"count": "five"})).is_err());
        assert!(tool.validate(&json!({"active": "yes"})).is_err());
    }

    #[tokio::test]
    async fn call_passthrough_succeeds() {
        let tool = SyntheticOutputTool::new();
        let input = json!({"result": "hello", "count": 3});
        let result = tool.call(input.clone(), &test_context()).await.unwrap();
        assert!(result.to_text().contains("successfully"));
        assert_eq!(result.structured_output.unwrap(), input);
    }

    #[tokio::test]
    async fn call_with_schema_valid() {
        let tool = SyntheticOutputTool::with_schema(json!({
            "type": "object",
            "properties": { "answer": { "type": "string" } },
            "required": ["answer"]
        }));
        let input = json!({"answer": "42"});
        let result = tool.call(input, &test_context()).await.unwrap();
        assert!(result.to_text().contains("successfully"));
    }

    #[tokio::test]
    async fn call_with_schema_invalid_returns_error() {
        let tool = SyntheticOutputTool::with_schema(json!({
            "type": "object",
            "properties": { "answer": { "type": "string" } },
            "required": ["answer"]
        }));
        let input = json!({});
        let result = tool.call(input, &test_context()).await.unwrap();
        assert!(result.to_text().contains("validation failed"));
    }

    #[test]
    fn tool_metadata() {
        let tool = SyntheticOutputTool::new();
        assert_eq!(tool.name(), "SyntheticOutput");
        assert!(tool.is_read_only());
        assert!(tool.is_concurrency_safe());
        assert_eq!(tool.category(), ToolCategory::Session);
    }

    #[test]
    fn input_schema_passthrough() {
        let tool = SyntheticOutputTool::new();
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], true);
    }

    #[test]
    fn input_schema_with_custom() {
        let custom = json!({"type": "object", "properties": {"x": {"type": "number"}}});
        let tool = SyntheticOutputTool::with_schema(custom.clone());
        assert_eq!(tool.input_schema(), custom);
    }
}
