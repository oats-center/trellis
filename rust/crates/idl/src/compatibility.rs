use crate::{api_digest, selected_surface_digest, semantic::*};
use std::collections::{BTreeMap, BTreeSet};

/// One deterministic directional compatibility issue.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CompatibilityIssue {
    /// Stable API/action/type-field path and direction.
    pub path: String,
    /// Human-readable incompatibility reason.
    pub message: String,
}

/// Directional API compatibility result.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CompatibilityReport {
    /// Whether every compared semantic surface is compatible.
    pub compatible: bool,
    /// Deterministically ordered incompatibilities.
    pub issues: Vec<CompatibilityIssue>,
}

/// Resource migration compatibility result.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResourceCompatibilityReport {
    /// Whether the retained representation can satisfy the declaration.
    pub compatible: bool,
    /// Whether application-provided historical-to-current migration is required.
    pub migration_required: bool,
    /// Deterministically ordered incompatibilities.
    pub issues: Vec<CompatibilityIssue>,
}

struct Comparison<'a> {
    old_api: &'a ApiDefinition,
    new_api: &'a ApiDefinition,
    old_graph: &'a PackageGraph,
    new_graph: &'a PackageGraph,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SelectedCompatibilityCacheKey {
    consumer_digest: String,
    provider_digest: String,
    selected_surface_digest: String,
}

/// Reusable cache for directional selected-surface compatibility results.
#[derive(Default)]
pub struct SelectedCompatibilityCache {
    entries: BTreeMap<SelectedCompatibilityCacheKey, CompatibilityReport>,
}

impl SelectedCompatibilityCache {
    /// Compare a selected surface, reusing an exact semantic result when available.
    pub fn compare(
        &mut self,
        consumer: &PackageGraph,
        selection: &InteractionSelection,
        provider: &PackageGraph,
    ) -> miette::Result<CompatibilityReport> {
        let key = SelectedCompatibilityCacheKey {
            consumer_digest: consumer.root_digest().to_owned(),
            provider_digest: provider.root_digest().to_owned(),
            selected_surface_digest: selected_surface_digest(consumer, selection)?,
        };
        if let Some(report) = self.entries.get(&key) {
            return Ok(report.clone());
        }
        let report = compare_selected(consumer, selection, provider);
        self.entries.insert(key, report.clone());
        Ok(report)
    }

    /// Return the number of exact semantic comparisons retained by the cache.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Return whether no compatibility results are cached.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Typed resource commitment retained by the runtime provisioner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RetainedResourceActual {
    /// No resource is currently provisioned.
    Unavailable,
    /// Participant-local state is provisioned.
    State,
    /// A KV bucket with effective limits.
    Kv {
        /// Retained history depth.
        history: u64,
        /// Effective TTL in milliseconds; zero means unlimited.
        ttl_ms: u64,
        /// Effective value cap; `None` means unlimited.
        max_value_bytes: Option<u64>,
    },
    /// A blob store with effective limits.
    Store {
        /// Effective TTL in milliseconds; zero means unlimited.
        ttl_ms: u64,
        /// Effective object cap; `None` means unlimited.
        max_object_bytes: Option<u64>,
        /// Effective aggregate cap; `None` means unlimited.
        max_total_bytes: Option<u64>,
    },
    /// A private job queue is provisioned.
    Job,
    /// A durable event consumer is provisioned.
    Consumer,
}

/// Compare an old provider implementation with a replacement provider.
pub fn compare_implementation(
    previous: ResolvedApi<'_>,
    replacement: ResolvedApi<'_>,
) -> CompatibilityReport {
    let mut issues = Vec::new();
    let previous_api = previous.definition;
    let replacement_api = replacement.definition;
    if matches!(
        (
            api_digest(previous.graph, &previous_api.identity),
            api_digest(replacement.graph, &replacement_api.identity)
        ),
        (Ok(previous), Ok(replacement)) if previous == replacement
    ) {
        return report(issues);
    }
    if previous_api.identity != replacement_api.identity {
        issue(&mut issues, "api", "API identity changed");
        return report(issues);
    }
    for (id, action) in &previous_api.actions {
        match replacement_api.actions.get(id) {
            None => issue(
                &mut issues,
                format!("actions.{}.{}", kind(id.kind), id.name),
                "action was removed",
            ),
            Some(next) => {
                let path = format!("api.{}.{}.{}", previous_api.name, kind(id.kind), id.name);
                if id.kind == ActionKind::Event {
                    let comparison = Comparison {
                        old_api: previous_api,
                        new_api: replacement_api,
                        old_graph: previous.graph,
                        new_graph: replacement.graph,
                    };
                    compare_action(
                        action,
                        next,
                        InteractionDirection::Publish,
                        &comparison,
                        &format!("{path}.publish"),
                        &mut issues,
                    );
                    compare_action(
                        action,
                        next,
                        InteractionDirection::Subscribe,
                        &comparison,
                        &format!("{path}.subscribe"),
                        &mut issues,
                    );
                } else {
                    let direction = if id.kind == ActionKind::Operation {
                        InteractionDirection::Invoke
                    } else if id.kind == ActionKind::Rpc {
                        InteractionDirection::Call
                    } else {
                        InteractionDirection::Subscribe
                    };
                    compare_action(
                        action,
                        next,
                        direction,
                        &Comparison {
                            old_api: previous_api,
                            new_api: replacement_api,
                            old_graph: previous.graph,
                            new_graph: replacement.graph,
                        },
                        &path,
                        &mut issues,
                    );
                }
            }
        }
    }
    report(issues)
}

/// Compare a consumer's selected actions against a provider graph.
pub fn compare_selected(
    consumer: &PackageGraph,
    selection: &InteractionSelection,
    provider: &PackageGraph,
) -> CompatibilityReport {
    let mut issues = Vec::new();
    let Some(consumer_api) = consumer
        .packages()
        .values()
        .find_map(|package| package.apis().get(&selection.api))
    else {
        issue(&mut issues, "selected.api", "consumer API is unavailable");
        return report(issues);
    };
    let Some(provider_api) = provider
        .packages()
        .values()
        .find_map(|package| package.apis().get(&selection.api))
    else {
        issue(&mut issues, "selected.api", "provider API is unavailable");
        return report(issues);
    };
    if matches!(
        (
            selected_surface_digest(consumer, selection),
            selected_surface_digest(provider, selection)
        ),
        (Ok(consumer), Ok(provider)) if consumer == provider
    ) {
        return report(issues);
    }
    for selected in &selection.actions {
        let path = format!(
            "api.{}.{}.{}",
            consumer_api.name,
            kind(selected.action.kind),
            selected.action.name
        );
        match (
            consumer_api.actions.get(&selected.action),
            provider_api.actions.get(&selected.action),
        ) {
            (_, None) => issue(&mut issues, path, "selected provider action is unavailable"),
            (Some(old), Some(new)) => compare_action(
                old,
                new,
                selected.direction,
                &Comparison {
                    old_api: consumer_api,
                    new_api: provider_api,
                    old_graph: consumer,
                    new_graph: provider,
                },
                &path,
                &mut issues,
            ),
            (None, _) => issue(
                &mut issues,
                path,
                "consumer selection references an unknown action",
            ),
        }
    }
    report(issues)
}

fn compare_action(
    old: &ActionDefinition,
    new: &ActionDefinition,
    direction: InteractionDirection,
    comparison: &Comparison<'_>,
    path: &str,
    issues: &mut Vec<CompatibilityIssue>,
) {
    let Comparison {
        old_graph,
        new_graph,
        ..
    } = comparison;
    match (old, new) {
        (
            ActionDefinition::Rpc {
                input: a,
                output: b,
                download: d,
                pagination: p,
                ..
            },
            ActionDefinition::Rpc {
                input: x,
                output: y,
                download: w,
                pagination: q,
                ..
            },
        ) => {
            compare_schema(a, old_graph, x, new_graph, &format!("{path}.input"), issues);
            compare_schema(
                y,
                new_graph,
                b,
                old_graph,
                &format!("{path}.output"),
                issues,
            );
            if d != w || p != q {
                issue(issues, path, "RPC transfer or pagination contract changed");
            }
            compare_errors(old, new, comparison, path, issues);
        }
        (
            ActionDefinition::Operation {
                input: a,
                output: b,
                update: c,
                signals: e,
                upload: f,
                ..
            },
            ActionDefinition::Operation {
                input: x,
                output: y,
                update: z,
                signals: p,
                upload: q,
                ..
            },
        ) => {
            compare_schema(a, old_graph, x, new_graph, &format!("{path}.input"), issues);
            compare_schema(
                y,
                new_graph,
                b,
                old_graph,
                &format!("{path}.output"),
                issues,
            );
            if !optional_subset(z.as_ref(), new_graph, c.as_ref(), old_graph) {
                issue(
                    issues,
                    format!("{path}.update"),
                    "schema is directionally incompatible",
                );
            }
            for (name, old_signal) in e {
                match p.get(name) {
                    Some(new_signal)
                        if type_subset(
                            old_signal,
                            old_graph,
                            new_signal,
                            new_graph,
                            &mut BTreeSet::new(),
                        ) => {}
                    _ => issue(
                        issues,
                        format!("{path}.signal.{name}"),
                        "signal is unavailable or incompatible",
                    ),
                }
            }
            if f != q {
                issue(issues, path, "Operation upload contract changed");
            }
            compare_errors(old, new, comparison, path, issues);
        }
        (
            ActionDefinition::Event {
                payload: a,
                parameters: b,
            },
            ActionDefinition::Event {
                payload: x,
                parameters: y,
            },
        ) => {
            match direction {
                InteractionDirection::Publish => compare_schema(
                    a,
                    old_graph,
                    x,
                    new_graph,
                    &format!("{path}.payload"),
                    issues,
                ),
                InteractionDirection::Subscribe => compare_schema(
                    x,
                    new_graph,
                    a,
                    old_graph,
                    &format!("{path}.payload"),
                    issues,
                ),
                _ => issue(issues, path, "invalid event interaction direction"),
            }
            if b != y {
                issue(
                    issues,
                    format!("{path}.parameters"),
                    "event routing parameters changed",
                );
            }
        }
        (
            ActionDefinition::Feed { input: a, event: b },
            ActionDefinition::Feed { input: x, event: y },
        ) => {
            compare_schema(a, old_graph, x, new_graph, &format!("{path}.input"), issues);
            compare_schema(y, new_graph, b, old_graph, &format!("{path}.event"), issues);
        }
        _ => issue(issues, path, "action kind changed"),
    }
}

fn compare_schema(
    source: &TypeRef,
    source_graph: &PackageGraph,
    target: &TypeRef,
    target_graph: &PackageGraph,
    path: &str,
    issues: &mut Vec<CompatibilityIssue>,
) {
    if let Some(detail) = type_mismatch(
        source,
        source_graph,
        target,
        target_graph,
        &mut BTreeSet::new(),
    ) {
        issue(
            issues,
            format!("{path}{detail}"),
            "schema is directionally incompatible",
        );
    }
}

fn optional_subset(
    source: Option<&TypeRef>,
    source_graph: &PackageGraph,
    target: Option<&TypeRef>,
    target_graph: &PackageGraph,
) -> bool {
    match (source, target) {
        (None, None) => true,
        (Some(source), Some(target)) => type_subset(
            source,
            source_graph,
            target,
            target_graph,
            &mut BTreeSet::new(),
        ),
        _ => false,
    }
}

fn compare_errors(
    old: &ActionDefinition,
    new: &ActionDefinition,
    comparison: &Comparison<'_>,
    path: &str,
    issues: &mut Vec<CompatibilityIssue>,
) {
    let Comparison {
        old_api,
        new_api,
        old_graph,
        new_graph,
    } = comparison;
    let (Some(old_errors), Some(new_errors)) = (action_errors(old), action_errors(new)) else {
        return;
    };
    for name in old_errors.intersection(new_errors) {
        let compatible = match (&old_api.errors[name], &new_api.errors[name]) {
            (None, None) => true,
            (Some(old), Some(new)) => {
                type_subset(new, new_graph, old, old_graph, &mut BTreeSet::new())
            }
            _ => false,
        };
        if !compatible {
            issue(
                issues,
                format!("{path}.error.{name}.output"),
                "known error payload is directionally incompatible",
            );
        }
    }
}

fn action_errors(action: &ActionDefinition) -> Option<&BTreeSet<String>> {
    match action {
        ActionDefinition::Rpc { errors, .. } | ActionDefinition::Operation { errors, .. } => {
            Some(errors)
        }
        _ => None,
    }
}

fn type_subset(
    source: &TypeRef,
    source_graph: &PackageGraph,
    target: &TypeRef,
    target_graph: &PackageGraph,
    visited: &mut BTreeSet<(TypeRef, TypeRef)>,
) -> bool {
    type_mismatch(source, source_graph, target, target_graph, visited).is_none()
}

fn type_mismatch(
    source: &TypeRef,
    source_graph: &PackageGraph,
    target: &TypeRef,
    target_graph: &PackageGraph,
    visited: &mut BTreeSet<(TypeRef, TypeRef)>,
) -> Option<String> {
    if source.package != target.package || source.id != target.id {
        return Some(".type".to_owned());
    }
    if !visited.insert((source.clone(), target.clone())) {
        return None;
    }
    let Some(source) = source_graph
        .package(&source.package)
        .and_then(|package| package.types().get(&source.id))
    else {
        return Some(".type".to_owned());
    };
    let Some(target) = target_graph
        .package(&target.package)
        .and_then(|package| package.types().get(&target.id))
    else {
        return Some(".type".to_owned());
    };
    definition_mismatch(source, source_graph, target, target_graph, visited)
}

fn definition_mismatch(
    source: &TypeDefinition,
    source_graph: &PackageGraph,
    target: &TypeDefinition,
    target_graph: &PackageGraph,
    visited: &mut BTreeSet<(TypeRef, TypeRef)>,
) -> Option<String> {
    match (source, target) {
        (TypeDefinition::Alias(source), TypeDefinition::Alias(target)) => {
            expression_mismatch(source, source_graph, target, target_graph, visited)
        }
        (TypeDefinition::Enum(_), TypeDefinition::Enum(_)) => None,
        (TypeDefinition::Model(source), TypeDefinition::Model(target)) => {
            for (name, target) in target {
                match source.get(name) {
                    Some(source) if !target.optional && source.optional => {
                        return Some(format!(".field.{name}.required"));
                    }
                    Some(source) => {
                        if let Some(path) = expression_mismatch(
                            &source.ty,
                            source_graph,
                            &target.ty,
                            target_graph,
                            visited,
                        ) {
                            return Some(format!(".field.{name}{path}"));
                        }
                    }
                    None if !target.optional => return Some(format!(".field.{name}.missing")),
                    None => {}
                }
            }
            None
        }
        _ => Some(".kind".to_owned()),
    }
}

fn expression_mismatch(
    source: &TypeExpression,
    source_graph: &PackageGraph,
    target: &TypeExpression,
    target_graph: &PackageGraph,
    visited: &mut BTreeSet<(TypeRef, TypeRef)>,
) -> Option<String> {
    match (source, target) {
        (TypeExpression::Named(source), TypeExpression::Named(target)) => {
            type_mismatch(source, source_graph, target, target_graph, visited)
        }
        (TypeExpression::Primitive(a, x), TypeExpression::Primitive(b, y)) => {
            (a != b || !bounds_subset(x, y)).then(|| ".bounds".to_owned())
        }
        (TypeExpression::List(a, x), TypeExpression::List(b, y)) => {
            if !bounds_subset(x, y) {
                Some(".bounds".to_owned())
            } else {
                expression_mismatch(a, source_graph, b, target_graph, visited)
                    .map(|path| format!(".item{path}"))
            }
        }
        (TypeExpression::Map(a), TypeExpression::Map(b))
        | (TypeExpression::Nullable(a), TypeExpression::Nullable(b))
        | (TypeExpression::CursorPage(a), TypeExpression::CursorPage(b)) => {
            expression_mismatch(a, source_graph, b, target_graph, visited)
        }
        (source, TypeExpression::Nullable(target)) => {
            expression_mismatch(source, source_graph, target, target_graph, visited)
        }
        (TypeExpression::CursorQuery, TypeExpression::CursorQuery) => None,
        _ => Some(".kind".to_owned()),
    }
}

fn bounds_subset(source: &Bounds, target: &Bounds) -> bool {
    target.min.is_none_or(|minimum| {
        source.min.is_some_and(|value| {
            value
                .compare(minimum)
                .is_some_and(|ordering| ordering.is_ge())
        })
    }) && target.max.is_none_or(|maximum| {
        source.max.is_some_and(|value| {
            value
                .compare(maximum)
                .is_some_and(|ordering| ordering.is_le())
        })
    }) && target
        .min_count
        .is_none_or(|minimum| source.min_count.is_some_and(|value| value >= minimum))
        && target
            .max_count
            .is_none_or(|maximum| source.max_count.is_some_and(|value| value <= maximum))
}

/// Compare persisted resource representation evolution between declarations.
pub fn compare_resource_evolution(
    previous: ResolvedResource<'_>,
    replacement: ResolvedResource<'_>,
) -> ResourceCompatibilityReport {
    let mut issues = Vec::new();
    if std::mem::discriminant(previous.definition) != std::mem::discriminant(replacement.definition)
    {
        issue(&mut issues, "kind", "resource kind changed");
        return ResourceCompatibilityReport {
            compatible: false,
            migration_required: false,
            issues,
        };
    }
    if resource_optional(previous.definition) != resource_optional(replacement.definition) {
        issue(&mut issues, "optional", "resource optionality changed");
    }
    let (old_schema, old_version) = representation(previous.definition);
    let (new_schema, new_version) = representation(replacement.definition);
    let mut migration_required = false;
    match (old_schema, new_schema) {
        (None, None)
            if std::mem::discriminant(previous.definition)
                == std::mem::discriminant(replacement.definition) => {}
        (Some((old, old_accepts)), Some((new, new_accepts))) => {
            let old_schema = crate::json_schema(previous.graph, old).ok();
            let new_schema = crate::json_schema(replacement.graph, new).ok();
            if old_version == new_version && old_schema != new_schema {
                issue(
                    &mut issues,
                    "schema",
                    "schema changed without a representation version change",
                );
            }
            if new_version < old_version {
                issue(
                    &mut issues,
                    "version",
                    "representation version moved backwards",
                );
            }
            for historic in old_accepts {
                let old_historic = crate::json_schema(previous.graph, &historic.ty).ok();
                if !new_accepts.iter().any(|candidate| {
                    candidate.version == historic.version
                        && crate::json_schema(replacement.graph, &candidate.ty).ok() == old_historic
                }) {
                    issue(
                        &mut issues,
                        format!("accepts.{}", historic.version),
                        "replacement drops a retained historical representation",
                    );
                }
            }
            if old_version != new_version {
                migration_required = true;
                let accepts_previous = new_accepts.iter().any(|value| {
                    value.version == old_version
                        && crate::json_schema(replacement.graph, &value.ty).ok() == old_schema
                });
                if !accepts_previous {
                    issue(
                        &mut issues,
                        "accepts",
                        "replacement does not accept the previous representation",
                    );
                }
            }
        }
        _ => issue(&mut issues, "kind", "resource kind changed"),
    }
    ResourceCompatibilityReport {
        compatible: issues.is_empty(),
        migration_required,
        issues,
    }
}

/// Compare a resource declaration with the runtime's retained actual commitment.
pub fn compare_resource(
    declaration: &ResourceDefinition,
    actual: &RetainedResourceActual,
) -> ResourceCompatibilityReport {
    let mut issues = Vec::new();
    if matches!(actual, RetainedResourceActual::Unavailable) {
        if !declaration.optional() {
            issue(
                &mut issues,
                "availability",
                "required resource is unavailable",
            );
        }
        return ResourceCompatibilityReport {
            compatible: issues.is_empty(),
            migration_required: false,
            issues,
        };
    }
    match (declaration, actual) {
        (ResourceDefinition::State { .. }, RetainedResourceActual::State)
        | (ResourceDefinition::Job { .. }, RetainedResourceActual::Job)
        | (ResourceDefinition::Consumer { .. }, RetainedResourceActual::Consumer) => {}
        (
            ResourceDefinition::Kv {
                history,
                ttl_ms,
                desired_max_value,
                ..
            },
            RetainedResourceActual::Kv {
                history: actual_history,
                ttl_ms: actual_ttl,
                max_value_bytes,
            },
        ) => {
            if actual_history < history {
                issue(
                    &mut issues,
                    "history",
                    "retained history is below the declaration",
                );
            }
            compare_duration(&mut issues, "ttl", *ttl_ms, *actual_ttl);
            compare_capacity(
                &mut issues,
                "max_value",
                *desired_max_value,
                *max_value_bytes,
            );
        }
        (
            ResourceDefinition::Store {
                ttl_ms,
                desired_max_object,
                desired_max_total,
                ..
            },
            RetainedResourceActual::Store {
                ttl_ms: actual_ttl,
                max_object_bytes,
                max_total_bytes,
            },
        ) => {
            compare_duration(&mut issues, "ttl", *ttl_ms, *actual_ttl);
            compare_capacity(
                &mut issues,
                "max_object",
                *desired_max_object,
                *max_object_bytes,
            );
            compare_capacity(
                &mut issues,
                "max_total",
                *desired_max_total,
                *max_total_bytes,
            );
        }
        _ => issue(&mut issues, "kind", "retained resource kind differs"),
    }
    ResourceCompatibilityReport {
        compatible: issues.is_empty(),
        migration_required: false,
        issues,
    }
}

fn compare_duration(issues: &mut Vec<CompatibilityIssue>, path: &str, desired: u64, actual: u64) {
    if actual != 0 && (desired == 0 || actual < desired) {
        issue(issues, path, "retained duration is below the declaration");
    }
}

fn compare_capacity(
    issues: &mut Vec<CompatibilityIssue>,
    path: &str,
    desired: Option<u64>,
    actual: Option<u64>,
) {
    if actual.is_some_and(|actual| desired.is_none_or(|desired| actual < desired)) {
        issue(issues, path, "retained capacity is below the declaration");
    }
}

fn resource_optional(value: &ResourceDefinition) -> bool {
    value.optional()
}

fn representation(
    value: &ResourceDefinition,
) -> (Option<(&TypeRef, &[HistoricRepresentation])>, u32) {
    match value {
        ResourceDefinition::State {
            schema,
            version,
            accepts,
            ..
        }
        | ResourceDefinition::Kv {
            schema,
            version,
            accepts,
            ..
        } => (Some((schema, accepts)), *version),
        _ => (None, 0),
    }
}
fn issue(
    issues: &mut Vec<CompatibilityIssue>,
    path: impl Into<String>,
    message: impl Into<String>,
) {
    issues.push(CompatibilityIssue {
        path: path.into(),
        message: message.into(),
    });
}
fn report(mut issues: Vec<CompatibilityIssue>) -> CompatibilityReport {
    issues.sort();
    CompatibilityReport {
        compatible: issues.is_empty(),
        issues,
    }
}
fn kind(value: ActionKind) -> &'static str {
    match value {
        ActionKind::Rpc => "rpc",
        ActionKind::Operation => "operation",
        ActionKind::Event => "event",
        ActionKind::Feed => "feed",
    }
}
