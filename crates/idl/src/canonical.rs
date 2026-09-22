use crate::semantic::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use miette::miette;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
};

/// Canonical IDL projection mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CanonicalMode {
    /// Includes presentation metadata and excludes comments and source layout.
    Presentation,
    /// Excludes presentation-only metadata and is the sole semantic digest input.
    Semantic,
}

/// Render one package as deterministic, parseable canonical Trellis IDL.
pub fn canonical_package(
    graph: &PackageGraph,
    package: &PackageId,
    mode: CanonicalMode,
) -> miette::Result<String> {
    let package = graph
        .package(package)
        .ok_or_else(|| miette!("package '{package}' is absent from graph"))?;
    render(package, graph.packages(), mode)
}

/// Compute one package's semantic digest.
pub fn package_digest(graph: &PackageGraph, package: &PackageId) -> miette::Result<String> {
    let package = graph
        .package(package)
        .ok_or_else(|| miette!("package '{package}' is absent from graph"))?;
    digest_package(package, graph.packages())
}

/// Compute one API semantic digest.
pub fn api_digest(graph: &PackageGraph, api: &ApiId) -> miette::Result<String> {
    let (package, value) = find_api(graph, api)?;
    let mut projection = package.clone();
    projection.apis.retain(|id, _| id == api);
    projection.participants.clear();
    let mut local = BTreeSet::new();
    let mut external = BTreeSet::new();
    for reference in value.errors.values().flatten() {
        gather_reachable(reference, package, &mut local, &mut external);
    }
    for action in value.actions.values() {
        let mut references = Vec::new();
        action_refs(action, &mut references);
        for reference in references {
            gather_reachable(reference, package, &mut local, &mut external);
        }
    }
    projection.types.retain(|id, _| local.contains(id));
    projection
        .dependencies
        .retain(|id, _| external.contains(id));
    digest_package(&projection, graph.packages())
}

/// Compute one participant semantic digest.
pub fn participant_digest(
    graph: &PackageGraph,
    participant: &ParticipantId,
) -> miette::Result<String> {
    let needs = graph
        .participant_needs(participant)
        .ok_or_else(|| miette!("participant '{participant}' is absent from graph"))?;
    let mut projection = String::new();
    append_digest_field(&mut projection, participant.as_str());
    append_digest_field(&mut projection, needs.digest());
    Ok(sha256_base64url(&projection))
}

/// Compute the semantic digest of exactly one consumer-selected API surface.
pub fn selected_surface_digest(
    graph: &PackageGraph,
    selection: &InteractionSelection,
) -> miette::Result<String> {
    let (_, api) = find_api(graph, &selection.api)?;
    let mut output = String::new();
    append_digest_field(&mut output, selection.api.as_str());
    let mut actions = selection.actions.iter().collect::<Vec<_>>();
    actions.sort_by_key(|action| selection_key(action));
    for selection in actions {
        append_digest_field(&mut output, &selection_key(selection));
        let action = api.actions.get(&selection.action).ok_or_else(|| {
            miette!(
                "selected action '{} {}' is absent from API '{}'",
                raw_kind(selection.action.kind),
                selection.action.name,
                api.identity
            )
        })?;
        match action {
            ActionDefinition::Rpc {
                input,
                output: result,
                errors,
                download,
                pagination,
            } => {
                append_schema(&mut output, graph, input)?;
                append_schema(&mut output, graph, result)?;
                append_digest_field(&mut output, if *download { "download" } else { "" });
                append_digest_field(
                    &mut output,
                    if pagination.is_some() { "cursor" } else { "" },
                );
                append_errors(&mut output, graph, api, errors)?;
            }
            ActionDefinition::Operation {
                input,
                output: result,
                update,
                errors,
                signals,
                upload,
            } => {
                append_schema(&mut output, graph, input)?;
                append_schema(&mut output, graph, result)?;
                if let Some(update) = update {
                    append_schema(&mut output, graph, update)?;
                } else {
                    append_digest_field(&mut output, "");
                }
                for (name, signal) in signals {
                    append_digest_field(&mut output, name);
                    append_schema(&mut output, graph, signal)?;
                }
                append_digest_field(&mut output, if *upload { "upload" } else { "" });
                append_errors(&mut output, graph, api, errors)?;
            }
            ActionDefinition::Event {
                payload,
                parameters,
            } => {
                append_schema(&mut output, graph, payload)?;
                append_digest_field(
                    &mut output,
                    &serde_json::to_string(parameters).expect("event paths serialize"),
                );
            }
            ActionDefinition::Feed { input, event } => {
                append_schema(&mut output, graph, input)?;
                append_schema(&mut output, graph, event)?;
            }
        }
    }
    for capability in &selection.optional_capabilities {
        append_digest_field(&mut output, capability.as_str());
    }
    Ok(sha256_base64url(&output))
}

fn append_errors(
    output: &mut String,
    graph: &PackageGraph,
    api: &ApiDefinition,
    errors: &BTreeSet<String>,
) -> miette::Result<()> {
    for name in errors {
        append_digest_field(output, name);
        if let Some(payload) = &api.errors[name] {
            append_schema(output, graph, payload)?;
        } else {
            append_digest_field(output, "");
        }
    }
    Ok(())
}

fn append_schema(
    output: &mut String,
    graph: &PackageGraph,
    reference: &TypeRef,
) -> miette::Result<()> {
    append_digest_field(
        output,
        &serde_json::to_string(&crate::json_schema(graph, reference)?).expect("schema serializes"),
    );
    Ok(())
}

fn append_digest_field(output: &mut String, value: &str) {
    write!(output, "{}:", value.len()).unwrap();
    output.push_str(value);
}

/// Compute consent-text identity independently of authority identity.
pub fn capability_consent_digest(
    graph: &PackageGraph,
    capability: &CapabilityId,
) -> miette::Result<String> {
    let value = graph
        .packages()
        .values()
        .flat_map(|package| package.apis.values())
        .find_map(|api| api.capabilities.get(capability))
        .ok_or_else(|| miette!("capability '{capability}' is absent from graph"))?;
    let mut digest = Sha256::new();
    for value in [
        capability.as_str(),
        value.description.as_str(),
        value.consequence.as_str(),
    ] {
        let length = u32::try_from(value.len())
            .map_err(|_| miette!("capability consent field exceeds u32 length"))?;
        digest.update(length.to_be_bytes());
        digest.update(value.as_bytes());
    }
    Ok(URL_SAFE_NO_PAD.encode(digest.finalize()))
}

pub(crate) fn digest_package(
    package: &SemanticPackage,
    packages: &BTreeMap<PackageId, SemanticPackage>,
) -> miette::Result<String> {
    Ok(sha256_base64url(&render(
        package,
        packages,
        CanonicalMode::Semantic,
    )?))
}

fn sha256_base64url(value: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(value.as_bytes()))
}

fn render(
    package: &SemanticPackage,
    _packages: &BTreeMap<PackageId, SemanticPackage>,
    mode: CanonicalMode,
) -> miette::Result<String> {
    let imports = Imports::new(package);
    let mut output = format!("package {};\n", quote(package.identity.as_str()));
    for (index, (id, dependency)) in package.dependencies.iter().enumerate() {
        writeln!(
            output,
            "dependency d{index} {} version {} digest {};",
            quote(id.as_str()),
            quote(&dependency.version.to_string()),
            quote(&dependency.digest)
        )
        .unwrap();
    }
    if !package.dependencies.is_empty() {
        output.push('\n');
    }
    for ((owner, name), alias) in &imports.names {
        let dependency = package
            .dependencies
            .keys()
            .position(|id| id == owner)
            .ok_or_else(|| miette!("reference to non-direct package '{owner}'"))?;
        writeln!(output, "import {{ {name} as {alias} }} from d{dependency};").unwrap();
    }
    if !imports.names.is_empty() {
        output.push('\n');
    }
    for (id, definition) in &package.types {
        render_type_definition(&mut output, id, definition, &imports);
        output.push('\n');
    }
    for api in package.apis.values() {
        output.push_str(&render_api(api, mode, &imports));
        output.push('\n');
    }
    let companions = package
        .participants
        .values()
        .filter_map(|participant| {
            participant
                .companion
                .as_ref()
                .map(|value| value.participant.clone())
        })
        .collect::<BTreeSet<_>>();
    for participant in package
        .participants
        .values()
        .filter(|participant| !companions.contains(&participant.identity))
    {
        render_participant(&mut output, participant, mode, &imports, package, false);
        output.push('\n');
    }
    Ok(output)
}

#[derive(Default)]
struct Imports {
    names: BTreeMap<(PackageId, String), String>,
}

impl Imports {
    fn new(package: &SemanticPackage) -> Self {
        let mut refs = BTreeSet::new();
        package
            .types
            .values()
            .for_each(|value| collect_type_definition(value, &package.identity, &mut refs));
        for api in package.apis.values() {
            api.errors
                .values()
                .flatten()
                .for_each(|value| collect_ref(value, &package.identity, &mut refs));
            api.actions
                .values()
                .for_each(|value| collect_action(value, &package.identity, &mut refs));
        }
        for participant in package.participants.values() {
            for api in participant.implements.iter().chain(participant.uses.keys()) {
                if let Some((owner, name)) = split_api(api.as_str()) {
                    if owner != package.identity.as_str() {
                        refs.insert((PackageId::new(owner), name.to_owned()));
                    }
                }
            }
            participant
                .resources
                .values()
                .for_each(|value| collect_resource(value, &package.identity, &mut refs));
        }
        let names = refs
            .into_iter()
            .enumerate()
            .map(|(index, value)| {
                let alias = format!("i{index}_{}", value.1);
                (value, alias)
            })
            .collect();
        Self { names }
    }
    fn ty(&self, reference: &TypeRef) -> String {
        self.names
            .get(&(reference.package.clone(), reference.id.as_str().to_owned()))
            .cloned()
            .unwrap_or_else(|| reference.id.as_str().to_owned())
    }
    fn api(&self, id: &ApiId) -> String {
        split_api(id.as_str())
            .and_then(|(owner, name)| self.names.get(&(PackageId::new(owner), name.to_owned())))
            .cloned()
            .unwrap_or_else(|| {
                split_api(id.as_str())
                    .map_or_else(|| id.as_str().to_owned(), |(_, name)| name.to_owned())
            })
    }
}

fn render_type_definition(
    output: &mut String,
    id: &TypeId,
    value: &TypeDefinition,
    imports: &Imports,
) {
    match value {
        TypeDefinition::Model(fields) => {
            writeln!(output, "model {id} {{").unwrap();
            for (name, field) in fields {
                writeln!(
                    output,
                    "  {name}{}: {};",
                    if field.optional { "?" } else { "" },
                    render_expr(&field.ty, imports)
                )
                .unwrap();
            }
            output.push_str("}\n");
        }
        TypeDefinition::Enum(symbols) => {
            writeln!(output, "enum {id} {{").unwrap();
            for symbol in symbols {
                writeln!(output, "  {symbol};").unwrap();
            }
            output.push_str("}\n");
        }
        TypeDefinition::Alias(value) => {
            writeln!(output, "type {id} = {};", render_expr(value, imports)).unwrap()
        }
    }
}

fn render_expr(value: &TypeExpression, imports: &Imports) -> String {
    match value {
        TypeExpression::Primitive(primitive, bounds) => format!(
            "{}{}",
            primitive_name(*primitive),
            render_bounds(bounds, *primitive == Primitive::String)
        ),
        TypeExpression::Named(reference) => imports.ty(reference),
        TypeExpression::List(value, bounds) => format!(
            "list<{}>{}",
            render_expr(value, imports),
            render_bounds(bounds, false)
        ),
        TypeExpression::Map(value) => format!("map<{}>", render_expr(value, imports)),
        TypeExpression::Nullable(value) => format!("{} | null", render_expr(value, imports)),
        TypeExpression::CursorQuery => "CursorQuery".into(),
        TypeExpression::CursorPage(value) => format!("CursorPage<{}>", render_expr(value, imports)),
    }
}

fn render_bounds(value: &Bounds, string: bool) -> String {
    let mut parts = Vec::new();
    if let Some(value) = value.min {
        parts.push(format!("min={}", numeric_bound(value)));
    }
    if let Some(value) = value.max {
        parts.push(format!("max={}", numeric_bound(value)));
    }
    if let Some(value) = value.min_count {
        parts.push(format!(
            "{}={value}",
            if string { "min_length" } else { "min_items" }
        ));
    }
    if let Some(value) = value.max_count {
        parts.push(format!(
            "{}={value}",
            if string { "max_length" } else { "max_items" }
        ));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("({})", parts.join(", "))
    }
}

fn numeric_bound(value: NumericBound) -> String {
    match value {
        NumericBound::Integer(value) => value.to_string(),
        NumericBound::Number(0.0) => "0".to_owned(),
        NumericBound::Number(value) => value.to_string(),
    }
}

fn render_api(api: &ApiDefinition, mode: CanonicalMode, imports: &Imports) -> String {
    let mut output = format!("api {}@v{} {{\n", api.name, api.major);
    if mode == CanonicalMode::Presentation {
        writeln!(output, "  title {};", quote(&api.docs.title)).unwrap();
        writeln!(output, "  description {};", quote(&api.docs.description)).unwrap();
        if let Some(version) = &api.version {
            writeln!(output, "  version {};", quote(&version.to_string())).unwrap();
        }
    }
    for (name, payload) in &api.errors {
        if let Some(payload) = payload {
            writeln!(output, "  error {name}({});", imports.ty(payload)).unwrap();
        } else {
            writeln!(output, "  error {name};").unwrap();
        }
    }
    let mut actions = api.actions.iter().collect::<Vec<_>>();
    actions.sort_by_key(|(id, _)| format!("{} {}", raw_kind(id.kind), id.name));
    for (id, action) in actions {
        render_action(&mut output, id, action, imports);
    }
    output.push_str("  capabilities {\n");
    for (id, capability) in &api.capabilities {
        if capability.public {
            output.push_str("    public {\n");
        } else {
            writeln!(
                output,
                "    capability {} {{",
                id.as_str()
                    .rsplit_once("::")
                    .map_or(id.as_str(), |(_, name)| name)
            )
            .unwrap();
        }
        if mode == CanonicalMode::Presentation {
            if !capability.title.is_empty() {
                writeln!(output, "      title {};", quote(&capability.title)).unwrap();
            }
            if !capability.description.is_empty() {
                writeln!(
                    output,
                    "      description {};",
                    quote(&capability.description)
                )
                .unwrap();
            }
            if !capability.consequence.is_empty() {
                writeln!(
                    output,
                    "      consequence {};",
                    quote(&capability.consequence)
                )
                .unwrap();
            }
        } else if capability.public {
            output.push_str("      title \"_\";\n");
        } else {
            output.push_str("      title \"_\";\n");
            writeln!(
                output,
                "      description {};",
                quote(&capability.description)
            )
            .unwrap();
            writeln!(
                output,
                "      consequence {};",
                quote(&capability.consequence)
            )
            .unwrap();
        }
        output.push_str("      allows {\n");
        let mut allows = capability.allows.iter().collect::<Vec<_>>();
        allows.sort_by_key(|selection| selection_key(selection));
        for selection in allows {
            render_selection(&mut output, selection, "        ");
        }
        output.push_str("      }\n    }\n");
    }
    output.push_str("  }\n}\n");
    output
}

fn render_action(output: &mut String, id: &ActionId, value: &ActionDefinition, imports: &Imports) {
    writeln!(output, "  {} {} {{", raw_kind(id.kind), id.name).unwrap();
    match value {
        ActionDefinition::Rpc {
            input,
            output: result,
            errors,
            download,
            pagination,
        } => {
            writeln!(output, "    input {};", imports.ty(input)).unwrap();
            writeln!(output, "    output {};", imports.ty(result)).unwrap();
            render_errors(output, errors);
            if *download {
                output.push_str("    download;\n");
            }
            if pagination.is_some() {
                output.push_str("    pagination cursor;\n");
            }
        }
        ActionDefinition::Operation {
            input,
            output: result,
            update,
            errors,
            signals,
            upload,
        } => {
            writeln!(output, "    input {};", imports.ty(input)).unwrap();
            writeln!(output, "    output {};", imports.ty(result)).unwrap();
            if let Some(update) = update {
                writeln!(output, "    progress {};", imports.ty(update)).unwrap();
            }
            render_errors(output, errors);
            if !signals.is_empty() {
                output.push_str("    signals {\n");
                for (name, ty) in signals {
                    writeln!(output, "      {name} {};", imports.ty(ty)).unwrap();
                }
                output.push_str("    }\n");
            }
            if *upload {
                output.push_str("    upload;\n");
            }
        }
        ActionDefinition::Event {
            payload,
            parameters,
        } => {
            writeln!(output, "    payload {};", imports.ty(payload)).unwrap();
            if !parameters.is_empty() {
                writeln!(
                    output,
                    "    params [{}];",
                    parameters
                        .iter()
                        .map(|path| path.join("."))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
                .unwrap();
            }
        }
        ActionDefinition::Feed { input, event } => {
            writeln!(output, "    input {};", imports.ty(input)).unwrap();
            writeln!(output, "    event {};", imports.ty(event)).unwrap();
        }
    }
    output.push_str("  }\n");
}

fn render_errors(output: &mut String, errors: &BTreeSet<String>) {
    if !errors.is_empty() {
        writeln!(
            output,
            "    errors [{}];",
            errors.iter().cloned().collect::<Vec<_>>().join(", ")
        )
        .unwrap();
    }
}
fn render_selection(output: &mut String, value: &ActionSelection, indent: &str) {
    let prefix = match value.direction {
        InteractionDirection::Call => "rpc",
        InteractionDirection::Invoke => "operation",
        InteractionDirection::Publish => "publish event",
        InteractionDirection::Subscribe if value.action.kind == ActionKind::Event => {
            "subscribe event"
        }
        InteractionDirection::Subscribe => "feed",
    };
    writeln!(output, "{indent}{prefix} {};", value.action.name).unwrap();
}

fn render_participant(
    output: &mut String,
    value: &ParticipantDefinition,
    mode: CanonicalMode,
    imports: &Imports,
    package: &SemanticPackage,
    optional: bool,
) {
    let _ = mode;
    writeln!(
        output,
        "{}{} {} {{",
        participant_kind(value.kind),
        if optional { " optional" } else { "" },
        value.name
    )
    .unwrap();
    for api in &value.implements {
        writeln!(output, "  implements {};", imports.api(api)).unwrap();
    }
    for (api, selections) in &value.uses {
        writeln!(output, "  use {} {{", imports.api(api)).unwrap();
        let mut actions = selections.actions.iter().collect::<Vec<_>>();
        actions.sort_by_key(|selection| selection_key(selection));
        for selection in actions {
            render_selection(output, selection, "    ");
        }
        for capability in &selections.optional_capabilities {
            writeln!(
                output,
                "    optional capability {};",
                capability
                    .as_str()
                    .rsplit_once("::")
                    .map_or(capability.as_str(), |(_, name)| name)
            )
            .unwrap();
        }
        output.push_str("  }\n");
    }
    for (name, resource) in &value.resources {
        render_resource(output, name, resource, imports, mode);
    }
    if let Some(companion) = &value.companion {
        if let Some(child) = package.participants.get(&companion.participant) {
            render_participant(output, child, mode, imports, package, companion.optional);
        }
    }
    output.push_str("}\n");
}

fn render_resource(
    output: &mut String,
    name: &ResourceName,
    value: &ResourceDefinition,
    imports: &Imports,
    mode: CanonicalMode,
) {
    let optional = match value {
        ResourceDefinition::State { optional, .. }
        | ResourceDefinition::Kv { optional, .. }
        | ResourceDefinition::Store { optional, .. }
        | ResourceDefinition::Job { optional, .. }
        | ResourceDefinition::Consumer { optional, .. } => *optional,
    };
    let kind = match value {
        ResourceDefinition::State { .. } => "state",
        ResourceDefinition::Kv { .. } => "kv",
        ResourceDefinition::Store { .. } => "store",
        ResourceDefinition::Job { .. } => "job",
        ResourceDefinition::Consumer { .. } => "consumer",
    };
    writeln!(
        output,
        "  {kind}{} {name} {{",
        if optional { " optional" } else { "" }
    )
    .unwrap();
    let docs = match value {
        ResourceDefinition::State { docs, .. }
        | ResourceDefinition::Kv { docs, .. }
        | ResourceDefinition::Store { docs, .. }
        | ResourceDefinition::Job { docs, .. }
        | ResourceDefinition::Consumer { docs, .. } => docs,
    };
    writeln!(
        output,
        "    title {};\n    description {};",
        quote(if mode == CanonicalMode::Semantic {
            "_"
        } else {
            &docs.title
        }),
        quote(if mode == CanonicalMode::Semantic {
            "_"
        } else {
            &docs.description
        })
    )
    .unwrap();
    match value {
        ResourceDefinition::State {
            schema,
            version,
            accepts,
            ..
        } => {
            writeln!(
                output,
                "    schema {};\n    version {version};",
                imports.ty(schema)
            )
            .unwrap();
            render_accepts(output, accepts, imports);
        }
        ResourceDefinition::Kv {
            schema,
            version,
            accepts,
            history,
            ttl_ms,
            desired_max_value,
            ..
        } => {
            writeln!(
                output,
                "    schema {};\n    version {version};",
                imports.ty(schema)
            )
            .unwrap();
            render_accepts(output, accepts, imports);
            writeln!(output, "    history {history};\n    ttl {ttl_ms}ms;").unwrap();
            if let Some(value) = desired_max_value {
                writeln!(output, "    desired_max_value {value}B;").unwrap();
            }
        }
        ResourceDefinition::Store {
            ttl_ms,
            desired_max_object,
            desired_max_total,
            ..
        } => {
            writeln!(output, "    ttl {ttl_ms}ms;").unwrap();
            if let Some(value) = desired_max_object {
                writeln!(output, "    desired_max_object {value}B;").unwrap();
            }
            if let Some(value) = desired_max_total {
                writeln!(output, "    desired_max_total {value}B;").unwrap();
            }
        }
        ResourceDefinition::Job {
            payload,
            result,
            update,
            deadline_ms,
            retry,
            key_concurrency,
            ..
        } => {
            writeln!(output, "    payload {};", imports.ty(payload)).unwrap();
            if let Some(value) = result {
                writeln!(output, "    result {};", imports.ty(value)).unwrap();
            }
            if let Some(value) = update {
                writeln!(output, "    update {};", imports.ty(value)).unwrap();
            }
            if let Some(value) = deadline_ms {
                writeln!(output, "    deadline {value}ms;").unwrap();
            }
            render_retry(output, retry);
            if let Some(value) = key_concurrency {
                writeln!(
                    output,
                    "    key_concurrency {{\n      path {};\n      policy {};\n    }}",
                    value.path.join("."),
                    match value.policy {
                        KeyConcurrencyPolicy::Queue => "queue",
                        KeyConcurrencyPolicy::Reject => "reject",
                        KeyConcurrencyPolicy::Supersede => "supersede",
                    }
                )
                .unwrap();
            }
        }
        ResourceDefinition::Consumer {
            events,
            concurrency,
            replay,
            retry,
            ..
        } => {
            writeln!(
                output,
                "    events [{}];",
                events
                    .iter()
                    .map(|(api, event)| format!("{}.{}", imports.api(api), event))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
            .unwrap();
            writeln!(
                output,
                "    concurrency {concurrency};\n    replay {};",
                if *replay == Replay::New { "new" } else { "all" }
            )
            .unwrap();
            render_retry(output, retry);
        }
    }
    output.push_str("  }\n");
}

fn render_accepts(output: &mut String, accepts: &[HistoricRepresentation], imports: &Imports) {
    if !accepts.is_empty() {
        output.push_str("    accepts {\n");
        let mut accepts = accepts.iter().collect::<Vec<_>>();
        accepts.sort_by_key(|value| value.version.to_string());
        for value in accepts {
            writeln!(
                output,
                "      {}: {};",
                value.version,
                imports.ty(&value.ty)
            )
            .unwrap();
        }
        output.push_str("    }\n");
    }
}
fn selection_key(value: &ActionSelection) -> String {
    let prefix = match value.direction {
        InteractionDirection::Call => "rpc",
        InteractionDirection::Invoke => "operation",
        InteractionDirection::Publish => "publish event",
        InteractionDirection::Subscribe if value.action.kind == ActionKind::Event => {
            "subscribe event"
        }
        InteractionDirection::Subscribe => "feed",
    };
    format!("{prefix} {}", value.action.name)
}
fn render_retry(output: &mut String, retry: &Option<RetryPolicy>) {
    if let Some(retry) = retry {
        writeln!(
            output,
            "    retry {{\n      attempts {};\n      backoff [{}];\n    }}",
            retry.attempts,
            retry
                .backoff_ms
                .iter()
                .map(|value| format!("{value}ms"))
                .collect::<Vec<_>>()
                .join(", ")
        )
        .unwrap();
    }
}
fn collect_type_definition(
    value: &TypeDefinition,
    root: &PackageId,
    refs: &mut BTreeSet<(PackageId, String)>,
) {
    match value {
        TypeDefinition::Model(fields) => fields
            .values()
            .for_each(|field| collect_expr(&field.ty, root, refs)),
        TypeDefinition::Alias(value) => collect_expr(value, root, refs),
        TypeDefinition::Enum(_) => {}
    }
}
fn collect_expr(
    value: &TypeExpression,
    root: &PackageId,
    refs: &mut BTreeSet<(PackageId, String)>,
) {
    match value {
        TypeExpression::Named(value) => collect_ref(value, root, refs),
        TypeExpression::List(value, _)
        | TypeExpression::Map(value)
        | TypeExpression::Nullable(value)
        | TypeExpression::CursorPage(value) => collect_expr(value, root, refs),
        _ => {}
    }
}
fn collect_ref(value: &TypeRef, root: &PackageId, refs: &mut BTreeSet<(PackageId, String)>) {
    if &value.package != root {
        refs.insert((value.package.clone(), value.id.as_str().to_owned()));
    }
}
fn collect_action(
    value: &ActionDefinition,
    root: &PackageId,
    refs: &mut BTreeSet<(PackageId, String)>,
) {
    match value {
        ActionDefinition::Rpc { input, output, .. } => {
            collect_ref(input, root, refs);
            collect_ref(output, root, refs);
        }
        ActionDefinition::Operation {
            input,
            output,
            update,
            signals,
            ..
        } => {
            collect_ref(input, root, refs);
            collect_ref(output, root, refs);
            if let Some(value) = update {
                collect_ref(value, root, refs);
            }
            signals
                .values()
                .for_each(|value| collect_ref(value, root, refs));
        }
        ActionDefinition::Event { payload, .. } => collect_ref(payload, root, refs),
        ActionDefinition::Feed { input, event } => {
            collect_ref(input, root, refs);
            collect_ref(event, root, refs);
        }
    }
}

fn action_refs<'a>(value: &'a ActionDefinition, refs: &mut Vec<&'a TypeRef>) {
    match value {
        ActionDefinition::Rpc { input, output, .. } => refs.extend([input, output]),
        ActionDefinition::Operation {
            input,
            output,
            update,
            signals,
            ..
        } => {
            refs.extend([input, output]);
            refs.extend(update);
            refs.extend(signals.values());
        }
        ActionDefinition::Event { payload, .. } => refs.push(payload),
        ActionDefinition::Feed { input, event } => refs.extend([input, event]),
    }
}

fn gather_reachable(
    reference: &TypeRef,
    package: &SemanticPackage,
    local: &mut BTreeSet<TypeId>,
    external: &mut BTreeSet<PackageId>,
) {
    if reference.package != package.identity {
        external.insert(reference.package.clone());
        return;
    }
    if !local.insert(reference.id.clone()) {
        return;
    }
    match &package.types[&reference.id] {
        TypeDefinition::Model(fields) => {
            for field in fields.values() {
                gather_expr(&field.ty, package, local, external);
            }
        }
        TypeDefinition::Alias(value) => gather_expr(value, package, local, external),
        TypeDefinition::Enum(_) => {}
    }
}

fn gather_expr(
    value: &TypeExpression,
    package: &SemanticPackage,
    local: &mut BTreeSet<TypeId>,
    external: &mut BTreeSet<PackageId>,
) {
    match value {
        TypeExpression::Named(reference) => gather_reachable(reference, package, local, external),
        TypeExpression::List(value, _)
        | TypeExpression::Map(value)
        | TypeExpression::Nullable(value)
        | TypeExpression::CursorPage(value) => gather_expr(value, package, local, external),
        _ => {}
    }
}
fn collect_resource(
    value: &ResourceDefinition,
    root: &PackageId,
    refs: &mut BTreeSet<(PackageId, String)>,
) {
    match value {
        ResourceDefinition::State {
            schema, accepts, ..
        }
        | ResourceDefinition::Kv {
            schema, accepts, ..
        } => {
            collect_ref(schema, root, refs);
            accepts
                .iter()
                .for_each(|value| collect_ref(&value.ty, root, refs));
        }
        ResourceDefinition::Job {
            payload,
            result,
            update,
            ..
        } => {
            collect_ref(payload, root, refs);
            if let Some(value) = result {
                collect_ref(value, root, refs);
            }
            if let Some(value) = update {
                collect_ref(value, root, refs);
            }
        }
        _ => {}
    }
}
fn find_api<'a>(
    graph: &'a PackageGraph,
    id: &ApiId,
) -> miette::Result<(&'a SemanticPackage, &'a ApiDefinition)> {
    graph
        .packages()
        .values()
        .find_map(|package| package.apis.get(id).map(|api| (package, api)))
        .ok_or_else(|| miette!("API '{id}' is absent from graph"))
}
fn split_api(value: &str) -> Option<(&str, &str)> {
    let (owner, rest) = value.split_once('.')?;
    Some((owner, rest.split_once("@v")?.0))
}
fn raw_kind(value: ActionKind) -> &'static str {
    match value {
        ActionKind::Rpc => "rpc",
        ActionKind::Operation => "operation",
        ActionKind::Event => "event",
        ActionKind::Feed => "feed",
    }
}
fn participant_kind(value: ParticipantKind) -> &'static str {
    match value {
        ParticipantKind::Service => "service",
        ParticipantKind::Device => "device",
        ParticipantKind::App => "app",
        ParticipantKind::Agent => "agent",
    }
}
fn primitive_name(value: Primitive) -> &'static str {
    match value {
        Primitive::String => "string",
        Primitive::Bool => "bool",
        Primitive::Int32 => "int32",
        Primitive::Uint32 => "uint32",
        Primitive::Int64 => "int64",
        Primitive::Uint64 => "uint64",
        Primitive::Number => "number",
        Primitive::Bytes => "bytes",
        Primitive::Timestamp => "timestamp",
        Primitive::Ulid => "ulid",
    }
}
fn quote(value: &str) -> String {
    serde_json::to_string(value).expect("strings serialize")
}
