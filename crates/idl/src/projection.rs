use crate::semantic::*;
use miette::miette;
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;

/// Project a resolved type and its reachable definitions to Draft 2020-12 JSON Schema.
pub fn json_schema(graph: &PackageGraph, root: &TypeRef) -> miette::Result<Value> {
    let mut definitions = Map::new();
    let mut visiting = BTreeSet::new();
    project_named(graph, root, &mut definitions, &mut visiting)?;
    Ok(json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$ref": format!("#/$defs/{}", definition_key(root)),
        "$defs": definitions,
    }))
}

fn project_named(
    graph: &PackageGraph,
    reference: &TypeRef,
    definitions: &mut Map<String, Value>,
    visiting: &mut BTreeSet<TypeRef>,
) -> miette::Result<()> {
    let key = definition_key(reference);
    if definitions.contains_key(&key) || !visiting.insert(reference.clone()) {
        return Ok(());
    }
    let package = graph
        .package(&reference.package)
        .ok_or_else(|| miette!("package '{}' is absent from graph", reference.package))?;
    let definition = package.types().get(&reference.id).ok_or_else(|| {
        miette!(
            "type '{}' is absent from package '{}'",
            reference.id,
            reference.package
        )
    })?;
    let value = match definition {
        TypeDefinition::Model(fields) => {
            let mut properties = Map::new();
            let mut required = Vec::new();
            for (name, field) in fields {
                properties.insert(
                    name.clone(),
                    project_expr(graph, &field.ty, definitions, visiting)?,
                );
                if !field.optional {
                    required.push(Value::String(name.clone()));
                }
            }
            json!({"type":"object", "properties":properties, "required":required, "additionalProperties":true})
        }
        TypeDefinition::Enum(symbols) => json!({"type":"string", "x-trellis-symbols":symbols}),
        TypeDefinition::Alias(value) => project_expr(graph, value, definitions, visiting)?,
    };
    visiting.remove(reference);
    definitions.insert(key, value);
    Ok(())
}

fn project_expr(
    graph: &PackageGraph,
    value: &TypeExpression,
    definitions: &mut Map<String, Value>,
    visiting: &mut BTreeSet<TypeRef>,
) -> miette::Result<Value> {
    Ok(match value {
        TypeExpression::Named(reference) => {
            project_named(graph, reference, definitions, visiting)?;
            json!({"$ref":format!("#/$defs/{}", definition_key(reference))})
        }
        TypeExpression::Primitive(primitive, bounds) => primitive_schema(*primitive, bounds),
        TypeExpression::List(member, bounds) => {
            let mut value = json!({"type":"array", "items":project_expr(graph, member, definitions, visiting)?});
            apply_count(&mut value, bounds, "minItems", "maxItems");
            value
        }
        TypeExpression::Map(member) => {
            json!({"type":"object", "additionalProperties":project_expr(graph, member, definitions, visiting)?})
        }
        TypeExpression::Nullable(member) => {
            json!({"anyOf":[project_expr(graph, member, definitions, visiting)?, {"type":"null"}]})
        }
        TypeExpression::CursorQuery => {
            json!({"type":"object", "properties":{"cursor":{"type":"string","minLength":1},"limit":{"type":"integer","minimum":1,"maximum":9_007_199_254_740_991_u64}}, "additionalProperties":true})
        }
        TypeExpression::CursorPage(member) => {
            json!({"type":"object", "properties":{"items":{"type":"array","items":project_expr(graph, member, definitions, visiting)?},"page":{"type":"object","properties":{"nextCursor":{"type":"string","minLength":1}},"additionalProperties":true}}, "required":["items","page"], "additionalProperties":true})
        }
    })
}

fn primitive_schema(value: Primitive, bounds: &Bounds) -> Value {
    let mut schema = match value {
        Primitive::String => json!({"type":"string"}),
        Primitive::Bool => json!({"type":"boolean"}),
        Primitive::Int32 => json!({"type":"integer","minimum":i32::MIN,"maximum":i32::MAX}),
        Primitive::Uint32 => json!({"type":"integer","minimum":0,"maximum":u32::MAX}),
        Primitive::Int64 => {
            json!({"type":"string","pattern":"^(0|-?[1-9][0-9]*)$","x-trellis-integer":"int64","x-trellis-minimum":"-9223372036854775808","x-trellis-maximum":"9223372036854775807"})
        }
        Primitive::Uint64 => {
            json!({"type":"string","pattern":"^(0|[1-9][0-9]*)$","x-trellis-integer":"uint64","x-trellis-minimum":"0","x-trellis-maximum":"18446744073709551615"})
        }
        Primitive::Number => json!({"type":"number"}),
        Primitive::Bytes => {
            json!({"type":"string","contentEncoding":"base64","x-trellis-base64":"standard-padded"})
        }
        Primitive::Timestamp => {
            json!({"type":"string","format":"date-time","pattern":r"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]{1,9})?Z$","x-trellis-canonical":"utc-rfc3339"})
        }
        Primitive::Ulid => json!({"type":"string","pattern":"^[0-7][0-9A-HJKMNP-TV-Z]{25}$"}),
    };
    if matches!(value, Primitive::String) {
        apply_count(&mut schema, bounds, "minLength", "maxLength");
    }
    if matches!(value, Primitive::Int64 | Primitive::Uint64) {
        if let Some(NumericBound::Integer(min)) = bounds.min {
            schema["x-trellis-minimum"] = json!(min.to_string());
        }
        if let Some(NumericBound::Integer(max)) = bounds.max {
            schema["x-trellis-maximum"] = json!(max.to_string());
        }
    } else {
        if let Some(min) = bounds.min {
            schema["minimum"] = numeric_json(min);
        }
        if let Some(max) = bounds.max {
            schema["maximum"] = numeric_json(max);
        }
    }
    schema
}

fn numeric_json(value: NumericBound) -> Value {
    match value {
        NumericBound::Integer(value) => {
            if let Ok(value) = i64::try_from(value) {
                json!(value)
            } else {
                json!(u64::try_from(value).expect("validated unsigned bound"))
            }
        }
        NumericBound::Number(value) => json!(value),
    }
}

fn apply_count(value: &mut Value, bounds: &Bounds, min: &str, max: &str) {
    if let Some(count) = bounds.min_count {
        value[min] = json!(count);
    }
    if let Some(count) = bounds.max_count {
        value[max] = json!(count);
    }
}
fn definition_key(reference: &TypeRef) -> String {
    format!("{}.{}", reference.package, reference.id)
}
