//! Plain-text documentation that code generators attach to generated items.
//!
//! Each function returns paragraphs: the authored `///` description or
//! title/description first, then notes the IDL states about the item, such as
//! optionality, declared constraints, errors, pagination, and the capabilities
//! that allow an action. The text is plain, not Markdown; each generator escapes
//! it for its own comment syntax.

use crate::{
    ActionDefinition, ActionId, ActionKind, ApiDefinition, Bounds, InteractionDirection,
    ModelField, NumericBound, PackageGraph, PackageId, Pagination, Primitive, TypeDefinition,
    TypeExpression,
};

/// Paragraphs documenting one package type: its authored description and, for
/// a constrained alias, the limits values must satisfy.
pub fn type_documentation(
    graph: &PackageGraph,
    package: &PackageId,
    name: &str,
    definition: &TypeDefinition,
) -> Vec<String> {
    let mut paragraphs = authored(graph, package, &format!("type.{name}"));
    if let TypeDefinition::Alias(expression) = definition {
        paragraphs.extend(value_notes(graph, expression));
    }
    paragraphs
}

/// Paragraphs documenting one model field: its authored description, whether
/// it may be absent or null, and the limits its declared type sets.
pub fn field_documentation(
    graph: &PackageGraph,
    package: &PackageId,
    model: &str,
    name: &str,
    field: &ModelField,
) -> Vec<String> {
    let mut paragraphs = authored(graph, package, &format!("type.{model}.{name}"));
    if field.optional {
        paragraphs.push("Optional: may be absent.".to_owned());
    }
    paragraphs.extend(value_notes(graph, &field.ty));
    paragraphs
}

/// Paragraphs documenting one API: its title and description.
pub fn api_documentation(api: &ApiDefinition) -> Vec<String> {
    let docs = api.documentation();
    [&docs.title, &docs.description]
        .into_iter()
        .filter(|text| !text.is_empty())
        .cloned()
        .collect()
}

/// Paragraphs documenting one API action: its authored description, whether
/// an RPC is paginated, its declared errors, and the capabilities that allow it.
pub fn action_documentation(
    graph: &PackageGraph,
    api: &ApiDefinition,
    id: &ActionId,
    action: &ActionDefinition,
) -> Vec<String> {
    let kind = match id.kind {
        ActionKind::Rpc => "rpc",
        ActionKind::Operation => "operation",
        ActionKind::Event => "event",
        ActionKind::Live => "live",
    };
    let key = format!("api.{}.{kind}.{}", api.name(), id.name);
    let mut paragraphs = graph
        .packages()
        .values()
        .find(|package| package.apis().contains_key(api.identity()))
        .and_then(|package| package.doc(&key))
        .map(|doc| vec![doc.to_owned()])
        .unwrap_or_default();
    let (errors, directions) = match action {
        ActionDefinition::Rpc {
            errors, pagination, ..
        } => {
            if let Some(Pagination::Cursor) = pagination {
                paragraphs.push(
                    "Paginated: each call returns one page of items; pass the page's next \
                    cursor back to read the following page."
                        .to_owned(),
                );
            }
            (Some(errors), &[InteractionDirection::Call][..])
        }
        ActionDefinition::Operation { errors, .. } => {
            (Some(errors), &[InteractionDirection::Invoke][..])
        }
        ActionDefinition::Event { .. } => (
            None,
            &[
                InteractionDirection::Publish,
                InteractionDirection::Subscribe,
            ][..],
        ),
        ActionDefinition::Live { .. } => (None, &[InteractionDirection::Subscribe][..]),
    };
    if let Some(errors) = errors.filter(|errors| !errors.is_empty()) {
        paragraphs.push(format!(
            "Errors: {}.",
            errors.iter().cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    for direction in directions {
        let verb = match direction {
            InteractionDirection::Call => "Call",
            InteractionDirection::Invoke => "Invoke",
            InteractionDirection::Publish => "Publish",
            InteractionDirection::Subscribe => "Subscribe",
        };
        for (capability_id, capability) in api.capabilities() {
            if capability
                .allows
                .iter()
                .any(|selection| &selection.action == id && selection.direction == *direction)
            {
                paragraphs.push(if capability.public {
                    format!("{verb}: public, no capability needed.")
                } else {
                    format!(
                        "{verb} is allowed by capability {capability_id} ({}): {}",
                        capability.title, capability.description
                    )
                });
            }
        }
    }
    paragraphs
}

fn authored(graph: &PackageGraph, package: &PackageId, key: &str) -> Vec<String> {
    graph
        .package(package)
        .and_then(|package| package.doc(key))
        .map(|doc| vec![doc.to_owned()])
        .unwrap_or_default()
}

/// Nullability and constraint notes for a value, following named aliases to
/// the constraints they declare.
fn value_notes(graph: &PackageGraph, expression: &TypeExpression) -> Vec<String> {
    let mut notes = Vec::new();
    let mut current = expression;
    let mut depth = 0;
    loop {
        match current {
            TypeExpression::Named(reference) if depth < 32 => {
                match graph
                    .package(&reference.package)
                    .and_then(|package| package.types().get(&reference.id))
                {
                    Some(TypeDefinition::Alias(inner)) => current = inner,
                    _ => break,
                }
            }
            TypeExpression::Nullable(inner) => {
                if !notes.iter().any(|note| note == NULLABLE) {
                    notes.push(NULLABLE.to_owned());
                }
                current = inner;
            }
            TypeExpression::Primitive(primitive, bounds) => {
                notes.extend(primitive_limit(*primitive, bounds));
                break;
            }
            TypeExpression::List(item, bounds) => {
                notes.extend(
                    range(bounds.min_count, bounds.max_count, "item", "items")
                        .map(|limit| format!("Items: {limit}.")),
                );
                let item_notes = value_notes(graph, item);
                notes.extend(
                    item_notes
                        .into_iter()
                        .map(|note| format!("Each item: {}", lower_first(&note))),
                );
                break;
            }
            _ => break,
        }
        depth += 1;
    }
    notes
}

const NULLABLE: &str = "Nullable: may be null.";

fn primitive_limit(primitive: Primitive, bounds: &Bounds) -> Option<String> {
    if primitive == Primitive::String {
        return range(
            bounds.min_count,
            bounds.max_count,
            "character",
            "characters",
        )
        .map(|limit| format!("Length: {limit} (Unicode code points)."));
    }
    let number = |bound: NumericBound| match bound {
        NumericBound::Integer(value) => value.to_string(),
        NumericBound::Number(value) => value.to_string(),
    };
    match (bounds.min.map(number), bounds.max.map(number)) {
        (Some(min), Some(max)) => Some(format!("Range: {min} to {max}, inclusive.")),
        (Some(min), None) => Some(format!("Range: at least {min}.")),
        (None, Some(max)) => Some(format!("Range: at most {max}.")),
        (None, None) => None,
    }
}

fn range(min: Option<u64>, max: Option<u64>, one: &str, many: &str) -> Option<String> {
    let unit = |count: u64| if count == 1 { one } else { many };
    match (min, max) {
        (Some(min), Some(max)) if min == max => Some(format!("exactly {min} {}", unit(min))),
        (Some(min), Some(max)) => Some(format!("{min} to {max} {}", unit(max))),
        (Some(min), None) => Some(format!("at least {min} {}", unit(min))),
        (None, Some(max)) => Some(format!("at most {max} {}", unit(max))),
        (None, None) => None,
    }
}

fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_lowercase().chain(chars).collect()
    })
}
