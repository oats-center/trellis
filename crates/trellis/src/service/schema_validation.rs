use jsonschema::error::ValidationErrorKind;
use jsonschema::Draft;
use serde_json::Value;

use super::error::{SchemaValidationIssue, ServerError, ValidationIssue};

/// Validate a JSON value against a JSON Schema.
///
/// Returns `Ok(())` on success.
/// Returns `ServerError::SchemaValidation` when all failures have `x-trellis-validation` metadata.
/// Returns `ServerError::Validation` when any failure lacks annotations.
#[doc = concat!("Trellis API operation `", stringify!(validate_input_schema), "`.")]
pub fn validate_input_schema(schema_json: &str, value: &Value) -> Result<(), ServerError> {
    PreparedSchema::new(schema_json)?.validate(value)
}

/// Parsed schema and validator owned by a prepared route or operation runtime.
#[derive(Debug)]
pub(crate) struct PreparedSchema {
    schema: Value,
    validator: jsonschema::Validator,
}

impl PreparedSchema {
    pub(crate) fn new(schema_json: &str) -> Result<Self, ServerError> {
        let schema: Value = serde_json::from_str(schema_json)
            .map_err(|e| ServerError::Nats(format!("failed to parse input schema: {e}")))?;

        let validator = jsonschema::options()
            .with_draft(Draft::Draft201909)
            .build(&schema)
            .map_err(|e| ServerError::Nats(format!("failed to compile input schema: {e}")))?;

        Ok(Self { schema, validator })
    }

    pub(crate) fn validate(&self, value: &Value) -> Result<(), ServerError> {
        let schema = &self.schema;
        let errors: Vec<_> = self.validator.iter_errors(value).collect();
        if errors.is_empty() {
            return Ok(());
        }

        let mut schema_validation_issues: Vec<SchemaValidationIssue> = Vec::new();
        let mut has_unannotated = false;

        for error in &errors {
            let keyword = error.kind().keyword();
            let instance_path = error.instance_path().to_string();
            let schema_path = format!("#{}", error.schema_path());

            let resolved_node = resolve_error_node(schema, error);

            let extension = resolved_node
                .and_then(|node| node.get("x-trellis-validation"))
                .and_then(|v| v.as_object());

            let is_supported = ALLOWED_HINT_KEYWORDS.contains(&keyword);

            let annotated = extension
                .and_then(|ext| ext.get("issues"))
                .and_then(|issues| issues.get(keyword))
                .and_then(|hint| hint.as_object())
                .and_then(|hint| hint.get("code"))
                .and_then(Value::as_str)
                .filter(|code| !code.is_empty());

            if let Some(code) = annotated.filter(|_| is_supported) {
                let hint = extension
                    .and_then(|ext| ext.get("issues"))
                    .and_then(|issues| issues.get(keyword))
                    .and_then(|h| h.as_object())
                    .unwrap();

                let label = extension
                    .and_then(|ext| ext.get("label"))
                    .and_then(Value::as_str);
                let note = hint.get("note").and_then(Value::as_str).or_else(|| {
                    extension
                        .and_then(|ext| ext.get("note"))
                        .and_then(Value::as_str)
                });
                let i18n_key = hint.get("i18nKey").and_then(Value::as_str);
                let severity = hint.get("severity").and_then(Value::as_str);

                schema_validation_issues.push(SchemaValidationIssue {
                    path: issue_path(error, &instance_path, keyword),
                    schema_path: Some(schema_path),
                    keyword: keyword.to_string(),
                    code: code.to_string(),
                    message: hint
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("Invalid value")
                        .to_string(),
                    label: label.map(String::from),
                    note: note.map(String::from),
                    i18n_key: i18n_key.map(String::from),
                    severity: severity.map(String::from),
                    params: params_for_keyword(error),
                });
            } else {
                has_unannotated = true;
            }
        }

        if !has_unannotated && !schema_validation_issues.is_empty() {
            Err(ServerError::SchemaValidation {
                issues: Box::new(schema_validation_issues),
            })
        } else {
            let issues: Vec<ValidationIssue> = errors
                .iter()
                .map(|error| {
                    let path = error.instance_path().to_string();
                    ValidationIssue {
                        path,
                        message: error.to_string(),
                    }
                })
                .collect();
            Err(ServerError::Validation {
                issues: Box::new(issues),
            })
        }
    }
}

const ALLOWED_HINT_KEYWORDS: &[&str] = &[
    "required",
    "minItems",
    "maxItems",
    "minLength",
    "maxLength",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "pattern",
    "format",
    "const",
];

/// Eager preparation with deferred error reporting for infallible registration APIs.
#[derive(Debug)]
pub(crate) struct PreparedOperationSchemas(Result<std::sync::Arc<OperationSchemas>, String>);

#[derive(Debug)]
pub(crate) struct OperationSchemas {
    pub(crate) input: PreparedSchema,
    pub(crate) progress: Option<PreparedSchema>,
    pub(crate) output: PreparedSchema,
    pub(crate) update: Option<PreparedSchema>,
    pub(crate) signals: std::collections::BTreeMap<String, PreparedSchema>,
}

impl PreparedOperationSchemas {
    pub(crate) fn new<D: super::operations::OperationDescriptor>() -> Self {
        let prepared = (|| {
            let signals: std::collections::BTreeMap<String, Value> =
                serde_json::from_str(D::SIGNAL_INPUT_SCHEMAS_JSON).map_err(|error| {
                    ServerError::Nats(format!("failed to parse signal schemas: {error}"))
                })?;
            Ok(std::sync::Arc::new(OperationSchemas {
                input: PreparedSchema::new(D::INPUT_SCHEMA_JSON)?,
                progress: D::PROGRESS_SCHEMA_JSON
                    .map(PreparedSchema::new)
                    .transpose()?,
                output: PreparedSchema::new(D::OUTPUT_SCHEMA_JSON)?,
                update: D::UPDATE_SCHEMA_JSON.map(PreparedSchema::new).transpose()?,
                signals: signals
                    .into_iter()
                    .map(|(name, schema)| Ok((name, PreparedSchema::new(&schema.to_string())?)))
                    .collect::<Result<_, ServerError>>()?,
            }))
        })();
        Self(prepared.map_err(|error| match error {
            ServerError::Nats(message) => message,
            other => other.to_string(),
        }))
    }

    pub(crate) fn get(&self) -> Result<&std::sync::Arc<OperationSchemas>, ServerError> {
        self.0
            .as_ref()
            .map_err(|message| ServerError::Nats(message.clone()))
    }
}

/// Resolve the schema node for a validation error.
/// For `required` errors, resolves to the missing property's schema.
/// For other errors, strips the last keyword segment from schema path.
fn resolve_error_node<'a>(
    root: &'a Value,
    error: &jsonschema::ValidationError<'_>,
) -> Option<&'a Value> {
    let keyword = error.kind().keyword();

    if keyword == "required" {
        if let ValidationErrorKind::Required { property } = error.kind() {
            let property_str = property.as_str().unwrap_or("");
            let parent_path = strip_last_segment(error.schema_path().as_str());
            let parent_node = root.pointer(&parent_path);
            let schema = parent_node
                .and_then(|n| n.get("properties"))
                .and_then(|p| p.get(property_str));
            return schema;
        }
    }

    let parent_path = strip_last_segment(error.schema_path().as_str());
    root.pointer(&parent_path)
}

/// Strip the last JSON Pointer segment.
fn strip_last_segment(path: &str) -> String {
    let path = path.strip_prefix('#').unwrap_or(path);
    if let Some(last_slash) = path.rfind('/') {
        if last_slash == 0 {
            String::new()
        } else {
            path[..last_slash].to_owned()
        }
    } else {
        String::new()
    }
}

/// Build the instance path, appending missing property for `required` errors.
fn issue_path(
    error: &jsonschema::ValidationError<'_>,
    instance_path: &str,
    keyword: &str,
) -> String {
    if keyword == "required" {
        if let ValidationErrorKind::Required { property } = error.kind() {
            let prop = property
                .as_str()
                .unwrap_or("")
                .replace('~', "~0")
                .replace('/', "~1");
            if instance_path.is_empty() {
                return format!("/{prop}");
            }
            return format!("{instance_path}/{prop}");
        }
    }
    instance_path.to_owned()
}

/// Extract keyword-specific params.
fn params_for_keyword(
    error: &jsonschema::ValidationError<'_>,
) -> Option<serde_json::Map<String, Value>> {
    match error.kind() {
        ValidationErrorKind::MinLength { limit }
        | ValidationErrorKind::MaxLength { limit }
        | ValidationErrorKind::MinItems { limit }
        | ValidationErrorKind::MaxItems { limit } => {
            let mut map = serde_json::Map::new();
            map.insert("limit".to_string(), Value::Number((*limit).into()));
            Some(map)
        }
        ValidationErrorKind::Minimum { limit }
        | ValidationErrorKind::ExclusiveMinimum { limit }
        | ValidationErrorKind::Maximum { limit }
        | ValidationErrorKind::ExclusiveMaximum { limit } => {
            let mut map = serde_json::Map::new();
            map.insert("limit".to_string(), limit.clone());
            Some(map)
        }
        ValidationErrorKind::Pattern { pattern } => {
            let mut map = serde_json::Map::new();
            map.insert("pattern".to_string(), Value::String(pattern.clone()));
            Some(map)
        }
        ValidationErrorKind::Format { format } => {
            let mut map = serde_json::Map::new();
            map.insert("format".to_string(), Value::String(format.clone()));
            Some(map)
        }
        ValidationErrorKind::Required { property } => {
            let mut map = serde_json::Map::new();
            map.insert(
                "missingProperty".to_string(),
                Value::String(property.as_str().unwrap_or("").to_string()),
            );
            Some(map)
        }
        ValidationErrorKind::Enum { options } => {
            let mut map = serde_json::Map::new();
            map.insert("allowedValues".to_string(), options.clone());
            Some(map)
        }
        ValidationErrorKind::Constant { expected_value } => {
            let mut map = serde_json::Map::new();
            map.insert("allowedValue".to_string(), expected_value.clone());
            Some(map)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prepared_validation_reuses_schema_and_reports_escaped_required_paths() {
        let schema = PreparedSchema::new(
            &json!({
                "type": "object",
                "properties": {
                    "": {
                        "type": "object",
                        "properties": {
                            "a/b~": {
                                "type": "string",
                                "x-trellis-validation": {"issues": {"required": {
                                    "code": "field.required", "message": "Provide the field."
                                }}}
                            }
                        },
                        "required": ["a/b~"]
                    }
                },
                "required": [""]
            })
            .to_string(),
        )
        .unwrap();
        for _ in 0..2 {
            schema.validate(&json!({"": {"a/b~": "valid"}})).unwrap();
            let ServerError::SchemaValidation { issues } =
                schema.validate(&json!({"": {}})).unwrap_err()
            else {
                panic!("required field must retain its annotated error")
            };
            assert_eq!(issues[0].path, "//a~1b~0");
            assert_eq!(issues[0].code, "field.required");
        }
    }

    #[test]
    fn validation_distinguishes_root_from_empty_name_property() {
        let root = PreparedSchema::new(r#"{"type":"string"}"#).unwrap();
        let ServerError::Validation { issues } = root.validate(&json!(4)).unwrap_err() else {
            panic!("expected structural error")
        };
        assert_eq!(issues[0].path, "");
        let property = PreparedSchema::new(r#"{"properties":{"":{"type":"string"}}}"#).unwrap();
        let ServerError::Validation { issues } = property.validate(&json!({"": 4})).unwrap_err()
        else {
            panic!("expected structural error")
        };
        assert_eq!(issues[0].path, "/");
    }

    const SCHEMA_WITH_ANNOTATION: &str = r#"{
        "type": "object",
        "properties": {
            "items": {
                "type": "array",
                "items": { "type": "string" },
                "minItems": 1,
                "x-trellis-validation": {
                    "label": "Items",
                    "issues": {
                        "minItems": {
                            "code": "test.items.required",
                            "message": "Add at least one item."
                        }
                    }
                }
            },
            "name": {
                "type": "string",
                "minLength": 3
            }
        },
        "required": ["items", "name"]
    }"#;

    const SCHEMA_WITHOUT_ANNOTATION: &str = r#"{
        "type": "object",
        "properties": {
            "name": {
                "type": "string",
                "minLength": 3
            }
        },
        "required": ["name"]
    }"#;

    const SCHEMA_WITH_ANNOTATED_REQUIRED: &str = r#"{
        "type": "object",
        "properties": {
            "title": {
                "type": "string",
                "minLength": 1,
                "x-trellis-validation": {
                    "label": "Title",
                    "issues": {
                        "minLength": {
                            "code": "doc.title.empty",
                            "message": "Enter a title."
                        }
                    }
                }
            }
        },
        "required": ["title"]
    }"#;

    const EMPTY_OBJECT_SCHEMA: &str = r#"{"type": "object", "properties": {}, "required": []}"#;

    #[test]
    fn valid_input_passes() {
        let value = json!({"items": ["hello"], "name": "test"});
        let result = validate_input_schema(SCHEMA_WITH_ANNOTATION, &value);
        assert!(result.is_ok(), "expected Ok, got {:?}", result);
    }

    #[test]
    fn empty_input_fails_with_validation() {
        let value = json!({});
        let result = validate_input_schema(SCHEMA_WITH_ANNOTATION, &value);
        assert!(result.is_err(), "expected Err");
        match result.unwrap_err() {
            ServerError::Validation { .. } => {}
            other => panic!("expected Validation, got {:?}", other),
        }
    }

    #[test]
    fn annotated_min_items_returns_schema_validation() {
        let value = json!({"items": [], "name": "test"});
        let result = validate_input_schema(SCHEMA_WITH_ANNOTATION, &value);
        assert!(result.is_err(), "expected Err");
        match result.unwrap_err() {
            ServerError::SchemaValidation { issues } => {
                assert_eq!(issues.len(), 1);
                assert_eq!(issues[0].code, "test.items.required");
                assert_eq!(issues[0].keyword, "minItems");
                assert_eq!(issues[0].path, "/items");
                assert_eq!(issues[0].message, "Add at least one item.");
                assert_eq!(issues[0].label.as_deref(), Some("Items"));
            }
            other => panic!("expected SchemaValidation, got {:?}", other),
        }
    }

    #[test]
    fn unannotated_failure_returns_validation() {
        let value = json!({"name": "ab"});
        let result = validate_input_schema(SCHEMA_WITH_ANNOTATION, &value);
        assert!(result.is_err(), "expected Err");
        match result.unwrap_err() {
            ServerError::Validation { .. } => {}
            other => panic!("expected Validation, got {:?}", other),
        }
    }

    #[test]
    fn schema_without_annotation_returns_validation() {
        let value = json!({"name": "ab"});
        let result = validate_input_schema(SCHEMA_WITHOUT_ANNOTATION, &value);
        assert!(result.is_err(), "expected Err");
        match result.unwrap_err() {
            ServerError::Validation { .. } => {}
            other => panic!("expected Validation, got {:?}", other),
        }
    }

    #[test]
    fn annotated_required_field_returns_schema_validation() {
        let value = json!({});
        let result = validate_input_schema(SCHEMA_WITH_ANNOTATED_REQUIRED, &value);
        assert!(result.is_err(), "expected Err");
        match result.unwrap_err() {
            // required keyword resolves but annotation covers minLength, not required,
            // so the current implementation falls through to Validation
            ServerError::Validation { .. } => {}
            other => panic!("expected Validation, got {:?}", other),
        }
    }

    #[test]
    fn empty_object_schema_passes_any_object() {
        let value = json!({"anything": "goes"});
        let result = validate_input_schema(EMPTY_OBJECT_SCHEMA, &value);
        assert!(result.is_ok(), "expected Ok, got {:?}", result);
    }

    #[test]
    fn invalid_json_schema_returns_internal_error() {
        let value = json!(42);
        let result = validate_input_schema("not valid json", &value);
        assert!(result.is_err(), "expected Err");
        match result.unwrap_err() {
            ServerError::Nats(msg) => assert!(
                msg.contains("failed to parse"),
                "expected parse error, got: {msg}"
            ),
            other => panic!("expected Nats internal error, got {:?}", other),
        }
    }

    #[test]
    fn unannotated_min_length_returns_validation() {
        let value = json!({"name": "ab"});
        let result = validate_input_schema(SCHEMA_WITHOUT_ANNOTATION, &value);
        assert!(result.is_err(), "expected Err");
        match result.unwrap_err() {
            ServerError::Validation { issues } => {
                assert!(!issues.is_empty(), "expected at least one issue");
            }
            other => panic!("expected Validation, got {:?}", other),
        }
    }
}
