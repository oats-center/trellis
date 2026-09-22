use crate::{
    ast::{self, Declaration, MemberValue, ParsedSource, ResourceValue, Spanned, TypeExpr},
    parser,
    project::PackageManifest,
    semantic::{
        ActionDefinition, ActionId, ActionKind, ActionSelection, ApiDefinition, ApiId, Bounds,
        CapabilityDefinition, CapabilityId, CompanionDefinition, Documentation,
        HistoricRepresentation, InteractionDirection, InteractionSelection, KeyConcurrency,
        KeyConcurrencyPolicy, NumericBound, PackageGraph, PackageId, Pagination,
        ParticipantDefinition, ParticipantId, ParticipantKind, ParticipantNeeds, Primitive, Replay,
        ResolvedDependency, ResourceDefinition, ResourceName, RetryPolicy, SemanticPackage,
        SourceSpan, SourceUnit, TypeDefinition, TypeExpression, TypeId, TypeRef,
    },
};
use miette::{miette, IntoDiagnostic};
use semver::VersionReq;
use std::collections::{BTreeMap, BTreeSet};
use trellis_protocol::{
    ApiSurfaceKind, GrantSet, ParticipantResourceKind, PermissionAction, PermissionAtom,
    PermissionTarget,
};

/// Dependency graphs supplied by acquisition tooling, keyed by manifest alias.
pub type SuppliedDependencies = BTreeMap<String, PackageGraph>;

type ResolvedDependencies = (
    BTreeMap<PackageId, SemanticPackage>,
    BTreeMap<PackageId, ResolvedDependency>,
    DependencyLookup,
);

pub(crate) fn compile(
    manifest: &PackageManifest,
    sources: Vec<SourceUnit>,
    supplied: SuppliedDependencies,
) -> miette::Result<PackageGraph> {
    manifest.validate()?;
    validate_source_set(manifest, &sources)?;
    let parsed = parser::parse(&sources)?;
    let (mut packages, dependencies, dependency_aliases) = dependencies(manifest, supplied)?;
    validate_preludes(manifest, &parsed, &dependencies)?;
    validate_imports(&parsed, &packages, &dependency_aliases)?;

    let declarations = collect(&parsed)?;
    let package_id = PackageId::new(&manifest.package.name);
    let mut package = SemanticPackage {
        identity: package_id.clone(),
        version: manifest.package.version.clone(),
        dependencies,
        types: BTreeMap::new(),
        apis: BTreeMap::new(),
        participants: BTreeMap::new(),
        sources: BTreeMap::new(),
    };
    let scope = Scope {
        package: &package_id,
        parsed: &parsed,
        declarations: &declarations,
        dependencies: &packages,
        dependency_aliases: &dependency_aliases,
    };

    for (name, declaration) in &declarations.types {
        let (source, declaration) = declaration;
        let resolved = (|| -> miette::Result<TypeDefinition> {
            Ok(match &declaration.value {
                Declaration::Model(model) => TypeDefinition::Model(
                    model
                        .fields
                        .iter()
                        .map(|(field, value)| {
                            if has_constraints(&value.ty) {
                                return Err(miette!(
                                    "constraints require a package-level named type alias"
                                ));
                            }
                            Ok((
                                field.clone(),
                                crate::semantic::ModelField {
                                    optional: value.optional,
                                    ty: resolve_type(&value.ty, *source, &scope)?,
                                    recursive: false,
                                },
                            ))
                        })
                        .collect::<miette::Result<_>>()?,
                ),
                Declaration::Enum(value) => {
                    TypeDefinition::Enum(value.symbols.iter().cloned().collect())
                }
                Declaration::Alias(value) => {
                    validate_alias_constraints(&value.ty)?;
                    TypeDefinition::Alias(resolve_type(&value.ty, *source, &scope)?)
                }
                _ => unreachable!(),
            })
        })();
        let value =
            resolved.map_err(|error| at(&parsed[*source], declaration, error.to_string()))?;
        package.types.insert(TypeId::new(name), value);
        record_span(&mut package, &parsed, format!("type.{name}"), declaration);
    }
    validate_type_graph(&package)?;
    mark_recursive_model_fields(&mut package);

    let scope = Scope {
        package: &package_id,
        parsed: &parsed,
        declarations: &declarations,
        dependencies: &packages,
        dependency_aliases: &dependency_aliases,
    };
    for (name, (source, declaration)) in &declarations.apis {
        let Declaration::Api(raw) = &declaration.value else {
            unreachable!()
        };
        let api = resolve_api(raw, *source, &scope, &package)
            .map_err(|error| at(&parsed[*source], declaration, error.to_string()))?;
        record_span(&mut package, &parsed, format!("api.{name}"), declaration);
        for action in api.actions.keys() {
            let raw_action = raw
                .actions
                .iter()
                .find(|candidate| {
                    candidate.kind == raw_action_kind(action.kind) && candidate.name == action.name
                })
                .expect("resolved action has syntax");
            record_inner_span(
                &mut package,
                &parsed,
                *source,
                format!(
                    "api.{name}.{}.{}",
                    raw_action_kind(action.kind),
                    action.name
                ),
                raw_action.span.clone(),
            );
        }
        for capability in api.capabilities.keys() {
            let raw_name = capability.as_str().rsplit("::").next().unwrap_or_default();
            let raw_capability = raw
                .capabilities
                .iter()
                .find(|candidate| candidate.name == raw_name)
                .expect("resolved capability has syntax");
            record_inner_span(
                &mut package,
                &parsed,
                *source,
                format!("api.{name}.capability.{capability}"),
                raw_capability.span.clone(),
            );
        }
        package.apis.insert(api.identity.clone(), api);
    }
    validate_cursor_usage(&package, &packages)?;

    let scope = Scope {
        package: &package_id,
        parsed: &parsed,
        declarations: &declarations,
        dependencies: &packages,
        dependency_aliases: &dependency_aliases,
    };
    for (source, declaration) in declarations.participants.values() {
        let Declaration::Participant(raw) = &declaration.value else {
            unreachable!()
        };
        let (participant, companions) =
            resolve_participant(raw, None, *source, &scope, &package)
                .map_err(|error| at(&parsed[*source], declaration, error.to_string()))?;
        record_participant_spans(&mut package, &parsed, *source, raw, None, &package_id);
        package
            .participants
            .insert(participant.identity.clone(), participant);
        for companion in companions {
            package
                .participants
                .insert(companion.identity.clone(), companion);
        }
    }

    packages.insert(package_id.clone(), package);
    let mut graph = PackageGraph {
        root: package_id,
        packages,
        digests: BTreeMap::new(),
        participant_needs: BTreeMap::new(),
    };
    for (id, package) in &graph.packages {
        let digest = if let Some(graph) = dependency_aliases
            .values()
            .find_map(|(_, graph)| (graph.root() == id).then_some(graph))
        {
            graph.root_digest().to_owned()
        } else {
            crate::canonical::digest_package(package, &graph.packages)?
        };
        graph.digests.insert(id.clone(), digest);
    }
    let participants = graph
        .packages
        .values()
        .flat_map(|package| package.participants.keys().cloned())
        .collect::<Vec<_>>();
    for participant in participants {
        let needs = derive_participant_needs(&graph, &participant)?;
        graph.participant_needs.insert(participant, needs);
    }
    Ok(graph)
}

fn record_participant_spans(
    package: &mut SemanticPackage,
    parsed: &[ParsedSource],
    source: usize,
    participant: &ast::Participant,
    parent: Option<&str>,
    package_id: &PackageId,
) {
    let lexical = parent.map_or_else(
        || participant.name.clone(),
        |parent| format!("{parent}.{}", participant.name),
    );
    let identity = format!("{package_id}.{lexical}");
    record_inner_span(
        package,
        parsed,
        source,
        format!("participant.{identity}"),
        participant.span.clone(),
    );
    for resource in &participant.resources {
        record_inner_span(
            package,
            parsed,
            source,
            format!("participant.{identity}.resource.{}", resource.name),
            resource.span.clone(),
        );
    }
    if let Some(companion) = &participant.companion {
        record_participant_spans(
            package,
            parsed,
            source,
            companion,
            Some(&lexical),
            package_id,
        );
    }
}

fn validate_source_set(manifest: &PackageManifest, sources: &[SourceUnit]) -> miette::Result<()> {
    let expected = manifest.sources.keys().collect::<BTreeSet<_>>();
    let actual = sources
        .iter()
        .map(|source| &source.alias)
        .collect::<BTreeSet<_>>();
    if expected != actual || actual.len() != sources.len() {
        return Err(miette!(
            "supplied sources must exactly match manifest source aliases"
        ));
    }
    for source in sources {
        if manifest.sources[&source.alias] != source.path.to_string_lossy() {
            return Err(miette!(
                "source '{}' path does not match the manifest",
                source.alias
            ));
        }
    }
    Ok(())
}

type DependencyLookup = BTreeMap<String, (PackageId, PackageGraph)>;

fn dependencies(
    manifest: &PackageManifest,
    supplied: SuppliedDependencies,
) -> miette::Result<ResolvedDependencies> {
    if supplied.keys().collect::<BTreeSet<_>>() != manifest.dependencies.keys().collect() {
        return Err(miette!(
            "supplied dependencies must exactly match manifest dependency aliases"
        ));
    }
    let mut packages = BTreeMap::<PackageId, SemanticPackage>::new();
    let mut digests = BTreeMap::<PackageId, String>::new();
    let mut direct = BTreeMap::new();
    let mut lookup = BTreeMap::new();
    for (alias, dependency) in &manifest.dependencies {
        let graph = supplied[alias].clone();
        if graph.root().as_str() != dependency.package {
            return Err(miette!(
                "dependency '{alias}' supplied package '{}' instead of '{}'",
                graph.root(),
                dependency.package
            ));
        }
        if graph
            .packages()
            .contains_key(&PackageId::new(&manifest.package.name))
        {
            return Err(miette!(
                "package dependency cycle through '{}'",
                dependency.package
            ));
        }
        if let Some(requirement) = &dependency.version {
            let requirement = VersionReq::parse(requirement).into_diagnostic()?;
            if !requirement.matches(graph.root_package().version()) {
                return Err(miette!(
                    "dependency '{alias}' version {} does not satisfy {requirement}",
                    graph.root_package().version()
                ));
            }
        }
        for (id, package) in graph.packages() {
            if let Some(existing) = packages.get(id) {
                let existing_digest = digests.get(id).map(String::as_str).unwrap_or_default();
                if existing.version() != package.version()
                    || existing_digest != graph.digest(id).unwrap_or_default()
                {
                    return Err(miette!(
                        "conflicting package '{}' in dependency closure",
                        id
                    ));
                }
            } else {
                packages.insert(id.clone(), package.clone());
                digests.insert(id.clone(), graph.digest(id).unwrap_or_default().to_owned());
            }
        }
        direct.insert(
            graph.root().clone(),
            ResolvedDependency {
                version: graph.root_package().version().clone(),
                digest: graph.root_digest().to_owned(),
            },
        );
        lookup.insert(alias.clone(), (graph.root().clone(), graph));
    }
    Ok((packages, direct, lookup))
}

fn validate_preludes(
    manifest: &PackageManifest,
    parsed: &[ParsedSource],
    dependencies: &BTreeMap<PackageId, ResolvedDependency>,
) -> miette::Result<()> {
    let present = parsed
        .iter()
        .filter(|source| {
            source.prelude.package.is_some() || !source.prelude.dependencies.is_empty()
        })
        .collect::<Vec<_>>();
    if present.is_empty() {
        return Ok(());
    }
    if present.len() != 1 {
        return Err(miette!("canonical package prelude may appear only once"));
    }
    let prelude = &present[0].prelude;
    if prelude.package.as_deref() != Some(&manifest.package.name) {
        return Err(miette!(
            "canonical package prelude disagrees with manifest identity"
        ));
    }
    let actual = prelude
        .dependencies
        .iter()
        .enumerate()
        .map(|(index, value)| {
            if value.alias != format!("d{index}") {
                return Err(miette!("canonical dependency aliases must be d0, d1, ..."));
            }
            Ok((
                value.package.as_str(),
                value.version.as_str(),
                value.digest.as_str(),
            ))
        })
        .collect::<miette::Result<BTreeSet<_>>>()?;
    if actual.len() != prelude.dependencies.len()
        || prelude.dependencies.len() != dependencies.len()
    {
        return Err(miette!(
            "canonical dependency prelude must contain each direct dependency exactly once"
        ));
    }
    let expected = dependencies
        .iter()
        .map(|(id, value)| {
            (
                id.as_str(),
                value.version.to_string(),
                value.digest.as_str(),
            )
        })
        .collect::<BTreeSet<_>>();
    let expected_refs = expected
        .iter()
        .map(|(id, version, digest)| (*id, version.as_str(), *digest))
        .collect::<BTreeSet<_>>();
    if actual != expected_refs {
        return Err(miette!(
            "canonical dependency prelude disagrees with supplied closure"
        ));
    }
    Ok(())
}

fn validate_imports(
    parsed: &[ParsedSource],
    packages: &BTreeMap<PackageId, SemanticPackage>,
    dependencies: &DependencyLookup,
) -> miette::Result<()> {
    for source in parsed {
        for (local, import) in &source.imports {
            if let Some(target) = parsed.iter().find(|unit| unit.alias == import.from) {
                if !target.declarations.iter().any(|declaration| {
                    declaration_name(&declaration.value) == import.name
                        && !matches!(declaration.value, Declaration::Participant(_))
                }) {
                    return Err(parser::diagnostic(
                        &SourceUnit {
                            alias: source.alias.clone(),
                            path: source.path.clone(),
                            source: source.text.clone(),
                        },
                        import.span.clone(),
                        format!(
                            "import '{local}' refers to missing type or API '{}' in source '{}'",
                            import.name, import.from
                        ),
                    ));
                }
                continue;
            }
            let Some((package, _)) = dependencies.get(&import.from) else {
                return Err(parser::diagnostic(
                    &SourceUnit {
                        alias: source.alias.clone(),
                        path: source.path.clone(),
                        source: source.text.clone(),
                    },
                    import.span.clone(),
                    format!("unknown import source '{}'", import.from),
                ));
            };
            let package_value = &packages[package];
            let ty = TypeId::new(&import.name);
            let exported_type = package_value.types.contains_key(&ty)
                && exported_types(package_value).contains(&ty);
            let api = package_value
                .apis
                .values()
                .any(|api| api.name == import.name);
            if !exported_type && !api {
                return Err(parser::diagnostic(
                    &SourceUnit {
                        alias: source.alias.clone(),
                        path: source.path.clone(),
                        source: source.text.clone(),
                    },
                    import.span.clone(),
                    format!(
                        "import '{local}' refers to unexported type or missing API '{}' in package '{}'",
                        import.name, package
                    ),
                ));
            }
        }
    }
    Ok(())
}

struct Declarations<'a> {
    types: BTreeMap<String, (usize, &'a Spanned<Declaration>)>,
    apis: BTreeMap<String, (usize, &'a Spanned<Declaration>)>,
    participants: BTreeMap<String, (usize, &'a Spanned<Declaration>)>,
}

fn collect(parsed: &[ParsedSource]) -> miette::Result<Declarations<'_>> {
    let mut result = Declarations {
        types: BTreeMap::new(),
        apis: BTreeMap::new(),
        participants: BTreeMap::new(),
    };
    let mut all = BTreeSet::new();
    for (source, unit) in parsed.iter().enumerate() {
        for declaration in &unit.declarations {
            let (name, target) = match &declaration.value {
                Declaration::Model(value) => {
                    if let Some(field) = value.fields.keys().find(|field| !lower_camel(field)) {
                        return Err(at(
                            unit,
                            declaration,
                            format!("model field '{field}' must be lowerCamel ASCII"),
                        ));
                    }
                    (&value.name, &mut result.types)
                }
                Declaration::Enum(value) => (&value.name, &mut result.types),
                Declaration::Alias(value) => (&value.name, &mut result.types),
                Declaration::Api(value) => (&value.name, &mut result.apis),
                Declaration::Participant(value) => (&value.name, &mut result.participants),
            };
            if !all.insert(name.clone()) {
                return Err(at(
                    unit,
                    declaration,
                    format!("duplicate package declaration '{name}'"),
                ));
            }
            if unit.imports.contains_key(name) {
                return Err(at(
                    unit,
                    declaration,
                    format!("declaration '{name}' collides with an imported name"),
                ));
            }
            target.insert(name.clone(), (source, declaration));
        }
    }
    Ok(result)
}

fn lower_camel(value: &str) -> bool {
    value
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase())
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

struct Scope<'a> {
    package: &'a PackageId,
    parsed: &'a [ParsedSource],
    declarations: &'a Declarations<'a>,
    dependencies: &'a BTreeMap<PackageId, SemanticPackage>,
    dependency_aliases: &'a DependencyLookup,
}

fn resolve_type(
    value: &TypeExpr,
    source: usize,
    scope: &Scope<'_>,
) -> miette::Result<TypeExpression> {
    Ok(match value {
        TypeExpr::Named(name) if name == "CursorQuery" => TypeExpression::CursorQuery,
        TypeExpr::Named(name) => TypeExpression::Named(resolve_type_ref(name, source, scope)?),
        TypeExpr::Primitive(name, constraints) => {
            TypeExpression::Primitive(primitive(name)?, bounds(name, constraints)?)
        }
        TypeExpr::List(member, constraints) => TypeExpression::List(
            Box::new(resolve_type(member, source, scope)?),
            bounds("list", constraints)?,
        ),
        TypeExpr::Map(member) => {
            TypeExpression::Map(Box::new(resolve_type(member, source, scope)?))
        }
        TypeExpr::Nullable(member) => {
            TypeExpression::Nullable(Box::new(resolve_type(member, source, scope)?))
        }
        TypeExpr::CursorPage(member) => {
            TypeExpression::CursorPage(Box::new(resolve_type(member, source, scope)?))
        }
    })
}

fn has_constraints(value: &TypeExpr) -> bool {
    match value {
        TypeExpr::Primitive(_, bounds) | TypeExpr::List(_, bounds) if !bounds.is_empty() => true,
        TypeExpr::List(member, _) => has_constraints(member),
        TypeExpr::Map(member) | TypeExpr::Nullable(member) | TypeExpr::CursorPage(member) => {
            has_constraints(member)
        }
        TypeExpr::Named(_) | TypeExpr::Primitive(_, _) => false,
    }
}

fn validate_alias_constraints(value: &TypeExpr) -> miette::Result<()> {
    let nested = match value {
        TypeExpr::Primitive(_, _) | TypeExpr::Named(_) => None,
        TypeExpr::List(member, _)
        | TypeExpr::Map(member)
        | TypeExpr::Nullable(member)
        | TypeExpr::CursorPage(member) => Some(member.as_ref()),
    };
    if nested.is_some_and(has_constraints) {
        return Err(miette!(
            "nested constraints require their own package-level named type alias"
        ));
    }
    Ok(())
}

fn primitive(name: &str) -> miette::Result<Primitive> {
    Ok(match name {
        "string" => Primitive::String,
        "bool" => Primitive::Bool,
        "int32" => Primitive::Int32,
        "uint32" => Primitive::Uint32,
        "int64" => Primitive::Int64,
        "uint64" => Primitive::Uint64,
        "number" => Primitive::Number,
        "bytes" => Primitive::Bytes,
        "timestamp" => Primitive::Timestamp,
        "ulid" => Primitive::Ulid,
        _ => return Err(miette!("unknown primitive '{name}'")),
    })
}

fn bounds(kind: &str, constraints: &[(String, String)]) -> miette::Result<Bounds> {
    let mut bounds = Bounds::default();
    for (name, value) in constraints {
        match name.as_str() {
            "min" | "max" if matches!(kind, "int32" | "uint32" | "int64" | "uint64" | "number") => {
                let number = if kind == "number" {
                    let value = value.parse::<f64>().into_diagnostic()?;
                    if !value.is_finite() {
                        return Err(miette!("numeric bounds must be finite"));
                    }
                    NumericBound::Number(value)
                } else {
                    NumericBound::Integer(value.parse::<i128>().into_diagnostic()?)
                };
                if name == "min" {
                    bounds.min = Some(number);
                } else {
                    bounds.max = Some(number);
                }
            }
            "min_length" | "max_length" if kind == "string" => set_count(&mut bounds, name, value)?,
            "min_items" | "max_items" if kind == "list" => set_count(&mut bounds, name, value)?,
            _ => return Err(miette!("constraint '{name}' is not valid for {kind}")),
        }
    }
    if bounds
        .min
        .zip(bounds.max)
        .is_some_and(|(min, max)| min.compare(max) == Some(std::cmp::Ordering::Greater))
        || bounds
            .min_count
            .zip(bounds.max_count)
            .is_some_and(|(min, max)| min > max)
    {
        return Err(miette!("minimum constraint exceeds maximum"));
    }
    match kind {
        "uint32" | "uint64"
            if bounds
                .min
                .into_iter()
                .chain(bounds.max)
                .any(|value| matches!(value, NumericBound::Integer(value) if value < 0)) =>
        {
            return Err(miette!("unsigned bounds cannot be negative"))
        }
        _ => {}
    }
    let domain = match kind {
        "int32" => Some((i32::MIN as i128, i32::MAX as i128)),
        "uint32" => Some((0, u32::MAX as i128)),
        "int64" => Some((i64::MIN as i128, i64::MAX as i128)),
        "uint64" => Some((0, u64::MAX as i128)),
        _ => None,
    };
    if domain.is_some_and(|(minimum, maximum)| {
        bounds
            .min
            .into_iter()
            .chain(bounds.max)
            .any(|value| matches!(value, NumericBound::Integer(value) if value < minimum || value > maximum))
    }) {
        return Err(miette!("integer bound is outside the {kind} domain"));
    }
    Ok(bounds)
}

fn set_count(bounds: &mut Bounds, name: &str, value: &str) -> miette::Result<()> {
    let value = value
        .parse::<u64>()
        .into_diagnostic()
        .map_err(|_| miette!("{name} must be a nonnegative integer"))?;
    if name.starts_with("min_") {
        bounds.min_count = Some(value);
    } else {
        bounds.max_count = Some(value);
    }
    Ok(())
}

fn resolve_type_ref(name: &str, source: usize, scope: &Scope<'_>) -> miette::Result<TypeRef> {
    let (package, name) = resolve_name(name, source, scope)?;
    if package == *scope.package {
        if !scope.declarations.types.contains_key(&name) {
            return Err(miette!("unknown type '{name}'"));
        }
    } else {
        let package_value = &scope.dependencies[&package];
        let id = TypeId::new(&name);
        if !package_value.types.contains_key(&id) || !exported_types(package_value).contains(&id) {
            return Err(miette!(
                "type '{name}' is not exported by package '{package}'"
            ));
        }
    }
    Ok(TypeRef {
        package,
        id: TypeId::new(name),
    })
}

fn resolve_name(
    name: &str,
    source: usize,
    scope: &Scope<'_>,
) -> miette::Result<(PackageId, String)> {
    if let Some(import) = scope.parsed[source].imports.get(name) {
        if let Some(target) = scope.parsed.iter().find(|unit| unit.alias == import.from) {
            if target
                .declarations
                .iter()
                .any(|declaration| declaration_name(&declaration.value) == import.name)
            {
                return Ok((scope.package.clone(), import.name.clone()));
            }
            return Err(miette!(
                "source '{}' does not declare '{}'",
                import.from,
                import.name
            ));
        }
        let Some((package, _)) = scope.dependency_aliases.get(&import.from) else {
            return Err(miette!("unknown import source '{}'", import.from));
        };
        return Ok((package.clone(), import.name.clone()));
    }
    if declaration_in_source(&scope.parsed[source], name) {
        return Ok((scope.package.clone(), name.to_owned()));
    }
    Err(miette!(
        "name '{name}' is not declared in this file or explicitly imported"
    ))
}

fn declaration_in_source(source: &ParsedSource, name: &str) -> bool {
    source
        .declarations
        .iter()
        .any(|value| declaration_name(&value.value) == name)
}
fn declaration_name(value: &Declaration) -> String {
    match value {
        Declaration::Model(v) => v.name.clone(),
        Declaration::Enum(v) => v.name.clone(),
        Declaration::Alias(v) => v.name.clone(),
        Declaration::Api(v) => v.name.clone(),
        Declaration::Participant(v) => v.name.clone(),
    }
}

fn resolve_api(
    raw: &ast::Api,
    source: usize,
    scope: &Scope<'_>,
    root: &SemanticPackage,
) -> miette::Result<ApiDefinition> {
    let canonical_unit = scope.parsed[source].prelude.package.is_some();
    if !canonical_unit && (raw.title.is_empty() || raw.description.is_empty()) {
        return Err(miette!(
            "API '{}' requires nonempty title and description",
            raw.name
        ));
    }
    if !raw.capabilities_present {
        return Err(miette!(
            "API '{}' requires exactly one capabilities block",
            raw.name
        ));
    }
    let identity = ApiId::new(format!("{}.{}@v{}", scope.package, raw.name, raw.major));
    trellis_protocol::validate_api_id(identity.as_str()).into_diagnostic()?;
    let version = raw
        .version
        .as_ref()
        .map(|value| value.parse())
        .transpose()
        .into_diagnostic()?;
    let errors = raw
        .errors
        .iter()
        .map(|(name, payload)| {
            Ok((
                name.clone(),
                payload
                    .as_ref()
                    .map(|value| resolve_type_ref(value, source, scope))
                    .transpose()?,
            ))
        })
        .collect::<miette::Result<_>>()?;
    let mut actions = BTreeMap::new();
    for raw_action in &raw.actions {
        let kind = action_kind(&raw_action.kind)?;
        let id = ActionId {
            kind,
            name: raw_action.name.clone(),
        };
        let action = resolve_action(raw_action, source, scope, root, &errors)?;
        validate_pagination(&action, root, scope.dependencies)?;
        if actions.insert(id.clone(), action).is_some() {
            return Err(miette!(
                "duplicate API action '{}.{}'",
                raw_action.kind,
                raw_action.name
            ));
        }
    }
    let mut capabilities = BTreeMap::new();
    for capability in &raw.capabilities {
        if !capability.public
            && (capability.title.is_empty()
                || capability.description.is_empty()
                || capability.consequence.is_empty()
                || capability.allows.is_empty())
        {
            return Err(miette!(
                "capability '{}' requires title, description, consequence, and nonempty allows",
                capability.name
            ));
        }
        let id = CapabilityId::new(format!("{identity}::{}", capability.name));
        let allows: BTreeSet<ActionSelection> = capability
            .allows
            .iter()
            .map(|selection| resolve_selection(selection, &actions))
            .collect::<miette::Result<_>>()?;
        if allows.len() != capability.allows.len() {
            return Err(miette!(
                "capability '{}' contains a duplicate allows target",
                capability.name
            ));
        }
        if capabilities
            .insert(
                id,
                CapabilityDefinition {
                    title: capability.title.clone(),
                    description: capability.description.clone(),
                    consequence: capability.consequence.clone(),
                    allows,
                    public: capability.public,
                },
            )
            .is_some()
        {
            return Err(miette!("duplicate capability '{}'", capability.name));
        }
    }
    if !capabilities.contains_key(&CapabilityId::new(format!("{identity}::public"))) {
        return Err(miette!(
            "API '{}' requires one public capability block",
            raw.name
        ));
    }
    for action in actions.keys() {
        let directions: &[InteractionDirection] = match action.kind {
            ActionKind::Rpc => &[InteractionDirection::Call],
            ActionKind::Operation => &[InteractionDirection::Invoke],
            ActionKind::Event => &[
                InteractionDirection::Publish,
                InteractionDirection::Subscribe,
            ],
            ActionKind::Feed => &[InteractionDirection::Subscribe],
        };
        for direction in directions {
            if !capabilities.values().any(|capability| {
                capability.allows.contains(&ActionSelection {
                    action: action.clone(),
                    direction: *direction,
                })
            }) {
                return Err(miette!(
                    "externally usable action '{} {}' direction '{direction:?}' is not covered by a capability",
                    raw_action_kind(action.kind),
                    action.name
                ));
            }
        }
    }
    let subjects = derive_api_subjects(&identity, &raw.name, raw.major, &actions)?;
    Ok(ApiDefinition {
        identity,
        name: raw.name.clone(),
        major: raw.major,
        version,
        docs: Documentation {
            title: raw.title.clone(),
            description: raw.description.clone(),
        },
        errors,
        actions,
        capabilities,
        subjects,
    })
}

fn derive_api_subjects(
    api_id: &ApiId,
    api_name: &str,
    major: u32,
    actions: &BTreeMap<ActionId, ActionDefinition>,
) -> miette::Result<trellis_protocol::DerivedApiSubjects> {
    let api_id = api_id.as_str();
    let version = format!("v{major}");
    let mut result = trellis_protocol::DerivedApiSubjects {
        rpc: BTreeMap::new(),
        operations: BTreeMap::new(),
        events: BTreeMap::new(),
        feeds: BTreeMap::new(),
    };
    for (id, action) in actions {
        let logical_name = format!("{api_name}.{}", id.name);
        let protocol = |value: Result<String, trellis_protocol::ProtocolError>| {
            value.map_err(|error| miette!(error.to_string()))
        };
        match action {
            ActionDefinition::Rpc { .. } => {
                result.rpc.insert(
                    id.name.clone(),
                    protocol(trellis_protocol::derive_rpc_subject(
                        &version,
                        &logical_name,
                    ))?,
                );
            }
            ActionDefinition::Operation { .. } => {
                result.operations.insert(
                    id.name.clone(),
                    protocol(trellis_protocol::derive_operation_subject(
                        &version,
                        &logical_name,
                    ))?,
                );
            }
            ActionDefinition::Event { parameters, .. } => {
                let base = protocol(trellis_protocol::derive_event_subject(api_id, &id.name))?;
                let mut template = base.clone();
                for path in parameters {
                    template.push_str(".{/");
                    template.push_str(&path.join("/"));
                    template.push('}');
                }
                result.events.insert(
                    id.name.clone(),
                    trellis_protocol::DerivedEventSubjects {
                        base,
                        template,
                        wildcard: protocol(trellis_protocol::derive_event_wildcard_subject(
                            api_id,
                            &id.name,
                            parameters.len(),
                        ))?,
                    },
                );
            }
            ActionDefinition::Feed { .. } => {
                result.feeds.insert(
                    id.name.clone(),
                    protocol(trellis_protocol::derive_feed_subject(
                        &version,
                        &logical_name,
                    ))?,
                );
            }
        }
    }
    Ok(result)
}

fn resolve_action(
    raw: &ast::Action,
    source: usize,
    scope: &Scope<'_>,
    root: &SemanticPackage,
    errors: &BTreeMap<String, Option<TypeRef>>,
) -> miette::Result<ActionDefinition> {
    let allowed: &[&str] = match raw.kind.as_str() {
        "rpc" => &["input", "output", "errors", "download", "pagination"],
        "operation" => &["input", "output", "progress", "errors", "signals", "upload"],
        "event" => &["payload", "params"],
        "feed" => &["input", "event"],
        _ => &[],
    };
    if let Some(member) = raw
        .members
        .keys()
        .find(|member| !allowed.contains(&member.as_str()))
    {
        return Err(miette!("{} action does not support '{member}'", raw.kind));
    }
    let type_member = |name: &str| -> miette::Result<TypeRef> {
        match raw.members.get(name) {
            Some(MemberValue::Name(value)) => resolve_type_ref(value, source, scope),
            _ => Err(miette!("{} '{}' requires '{name}'", raw.kind, raw.name)),
        }
    };
    let optional_type = |name: &str| -> miette::Result<Option<TypeRef>> {
        match raw.members.get(name) {
            Some(MemberValue::Name(value)) => Ok(Some(resolve_type_ref(value, source, scope)?)),
            None => Ok(None),
            _ => Err(miette!("invalid '{name}' member")),
        }
    };
    let domain_errors = match raw.members.get("errors") {
        Some(MemberValue::Names(values)) => {
            let resolved = values
                .iter()
                .map(|value| {
                    if errors.contains_key(value) {
                        Ok(value.clone())
                    } else {
                        Err(miette!("unknown error '{value}'"))
                    }
                })
                .collect::<miette::Result<BTreeSet<_>>>()?;
            if resolved.len() != values.len() {
                return Err(miette!("action error lists must not contain duplicates"));
            }
            resolved
        }
        None => BTreeSet::new(),
        _ => return Err(miette!("invalid errors member")),
    };
    Ok(match raw.kind.as_str() {
        "rpc" => ActionDefinition::Rpc {
            input: type_member("input")?,
            output: type_member("output")?,
            errors: domain_errors,
            download: raw.members.contains_key("download"),
            pagination: match raw.members.get("pagination") {
                None => None,
                Some(MemberValue::Name(value)) if value == "cursor" => Some(Pagination::Cursor),
                _ => return Err(miette!("pagination must be 'cursor'")),
            },
        },
        "operation" => {
            let signals = match raw.members.get("signals") {
                Some(MemberValue::Signals(values)) => values
                    .iter()
                    .map(|(name, ty)| Ok((name.clone(), resolve_type_ref(ty, source, scope)?)))
                    .collect::<miette::Result<_>>()?,
                None => BTreeMap::new(),
                _ => return Err(miette!("invalid signals member")),
            };
            ActionDefinition::Operation {
                input: type_member("input")?,
                output: type_member("output")?,
                update: optional_type("progress")?,
                errors: domain_errors,
                signals,
                upload: raw.members.contains_key("upload"),
            }
        }
        "event" => {
            let payload = type_member("payload")?;
            let parameters = match raw.members.get("params") {
                Some(MemberValue::Paths(value)) => value.clone(),
                None => Vec::new(),
                _ => return Err(miette!("invalid event params")),
            };
            if parameters.iter().collect::<BTreeSet<_>>().len() != parameters.len() {
                return Err(miette!("event parameter paths must be distinct"));
            }
            for path in &parameters {
                validate_scalar_path(&payload, path, scope.dependencies, root)?;
            }
            ActionDefinition::Event {
                payload,
                parameters,
            }
        }
        "feed" => ActionDefinition::Feed {
            input: type_member("input")?,
            event: type_member("event")?,
        },
        _ => unreachable!(),
    })
}

fn action_kind(value: &str) -> miette::Result<ActionKind> {
    Ok(match value {
        "rpc" => ActionKind::Rpc,
        "operation" => ActionKind::Operation,
        "event" => ActionKind::Event,
        "feed" => ActionKind::Feed,
        _ => return Err(miette!("unknown action kind '{value}'")),
    })
}

fn validate_pagination(
    action: &ActionDefinition,
    root: &SemanticPackage,
    dependencies: &BTreeMap<PackageId, SemanticPackage>,
) -> miette::Result<()> {
    let ActionDefinition::Rpc {
        input,
        output,
        pagination: Some(Pagination::Cursor),
        ..
    } = action
    else {
        return Ok(());
    };
    let TypeDefinition::Model(fields) = terminal_definition(input, root, dependencies)? else {
        return Err(miette!("cursor-paginated RPC input must be a model"));
    };
    let page = fields.get("page").ok_or_else(|| {
        miette!("cursor-paginated RPC input requires optional 'page: CursorQuery'")
    })?;
    if !page.optional || !matches!(page.ty, TypeExpression::CursorQuery) {
        return Err(miette!(
            "cursor-paginated RPC input requires optional 'page: CursorQuery'"
        ));
    }
    let TypeDefinition::Alias(TypeExpression::CursorPage(_)) =
        terminal_definition(output, root, dependencies)?
    else {
        return Err(miette!(
            "cursor-paginated RPC output must resolve to CursorPage<T>"
        ));
    };
    Ok(())
}

fn referenced_definition<'a>(
    reference: &TypeRef,
    root: &'a SemanticPackage,
    dependencies: &'a BTreeMap<PackageId, SemanticPackage>,
) -> miette::Result<&'a TypeDefinition> {
    package_for(root, dependencies, &reference.package)?
        .types
        .get(&reference.id)
        .ok_or_else(|| miette!("unknown type reference '{reference:?}'"))
}

fn terminal_definition<'a>(
    reference: &TypeRef,
    root: &'a SemanticPackage,
    dependencies: &'a BTreeMap<PackageId, SemanticPackage>,
) -> miette::Result<&'a TypeDefinition> {
    let mut reference = reference;
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(reference.clone()) {
            return Err(miette!("type alias cycle while resolving '{reference:?}'"));
        }
        let definition = referenced_definition(reference, root, dependencies)?;
        match definition {
            TypeDefinition::Alias(TypeExpression::Named(next)) => reference = next,
            _ => return Ok(definition),
        }
    }
}

fn terminal_reference(
    reference: &TypeRef,
    root: &SemanticPackage,
    dependencies: &BTreeMap<PackageId, SemanticPackage>,
) -> miette::Result<TypeRef> {
    let mut reference = reference.clone();
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(reference.clone()) {
            return Err(miette!("type alias cycle while resolving '{reference:?}'"));
        }
        match referenced_definition(&reference, root, dependencies)? {
            TypeDefinition::Alias(TypeExpression::Named(next)) => reference = next.clone(),
            _ => return Ok(reference),
        }
    }
}
fn raw_action_kind(value: ActionKind) -> &'static str {
    match value {
        ActionKind::Rpc => "rpc",
        ActionKind::Operation => "operation",
        ActionKind::Event => "event",
        ActionKind::Feed => "feed",
    }
}

fn resolve_selection(
    raw: &ast::Selection,
    actions: &BTreeMap<ActionId, ActionDefinition>,
) -> miette::Result<ActionSelection> {
    let action = ActionId {
        kind: action_kind(&raw.kind)?,
        name: raw.name.clone(),
    };
    if !actions.contains_key(&action) {
        return Err(miette!(
            "selected action '{} {}' does not exist",
            raw.kind,
            raw.name
        ));
    }
    let direction = match (raw.direction.as_str(), action.kind) {
        ("call", ActionKind::Rpc) => InteractionDirection::Call,
        ("invoke", ActionKind::Operation) => InteractionDirection::Invoke,
        ("publish", ActionKind::Event) => InteractionDirection::Publish,
        ("subscribe", ActionKind::Event | ActionKind::Feed) => InteractionDirection::Subscribe,
        _ => {
            return Err(miette!(
                "invalid direction '{}' for {}",
                raw.direction,
                raw.kind
            ))
        }
    };
    Ok(ActionSelection { action, direction })
}

fn resolve_api_ref(
    name: &str,
    source: usize,
    scope: &Scope<'_>,
    root: &SemanticPackage,
) -> miette::Result<(ApiId, ApiDefinition)> {
    let (package, name) = resolve_name(name, source, scope)?;
    let apis = if package == *scope.package {
        &root.apis
    } else {
        &scope.dependencies[&package].apis
    };
    apis.iter()
        .find(|(_, api)| api.name == name)
        .map(|(id, api)| (id.clone(), api.clone()))
        .ok_or_else(|| miette!("unknown API '{name}'"))
}

fn resolve_participant(
    raw: &ast::Participant,
    parent: Option<&str>,
    source: usize,
    scope: &Scope<'_>,
    root: &SemanticPackage,
) -> miette::Result<(ParticipantDefinition, Vec<ParticipantDefinition>)> {
    let kind = match raw.kind.as_str() {
        "service" => ParticipantKind::Service,
        "device" => ParticipantKind::Device,
        "app" => ParticipantKind::App,
        "agent" => ParticipantKind::Agent,
        _ => return Err(miette!("unknown participant kind '{}'", raw.kind)),
    };
    let lexical = parent.map_or_else(
        || raw.name.clone(),
        |parent| format!("{parent}.{}", raw.name),
    );
    let identity = ParticipantId::new(format!("{}.{}", scope.package, lexical));
    if !matches!(kind, ParticipantKind::Service | ParticipantKind::Device)
        && !raw.implements.is_empty()
    {
        return Err(miette!(
            "only service and device participants may implement APIs"
        ));
    }
    let mut implements = BTreeSet::new();
    for name in &raw.implements {
        let (api, _) = resolve_api_ref(name, source, scope, root)?;
        if !implements.insert(api) {
            return Err(miette!("participant implements an API more than once"));
        }
    }
    let mut uses = BTreeMap::<ApiId, InteractionSelection>::new();
    let mut used_apis = BTreeMap::new();
    for use_ in &raw.uses {
        let (api_id, api) = resolve_api_ref(&use_.api, source, scope, root)?;
        used_apis.insert(api_id.clone(), api.clone());
        let selected = uses
            .entry(api_id.clone())
            .or_insert_with(|| InteractionSelection {
                api: api_id,
                actions: BTreeSet::new(),
                optional_capabilities: BTreeSet::new(),
            });
        for selection in &use_.selections {
            if !selected
                .actions
                .insert(resolve_selection(selection, &api.actions)?)
            {
                return Err(miette!("interaction is selected more than once"));
            }
        }
        for name in &use_.optional_capabilities {
            let id = CapabilityId::new(format!("{}::{name}", api.identity));
            if !selected.optional_capabilities.insert(id) {
                return Err(miette!("optional capability is declared more than once"));
            }
        }
    }
    for (api_id, selected) in &uses {
        let api = &used_apis[api_id];
        for capability_id in &selected.optional_capabilities {
            let Some(capability) = api.capabilities.get(capability_id) else {
                return Err(miette!("unknown optional capability '{capability_id}'"));
            };
            if capability.public
                || !capability
                    .allows
                    .iter()
                    .any(|allow| selected.actions.contains(allow))
            {
                return Err(miette!(
                    "optional capability '{capability_id}' is not a named capability implicated by selected actions"
                ));
            }
        }
    }
    let mut resources = BTreeMap::new();
    for resource in &raw.resources {
        let definition = resolve_resource(resource, kind, source, scope, root)?;
        if resources
            .insert(ResourceName::new(&resource.name), definition)
            .is_some()
        {
            return Err(miette!("duplicate resource '{}'", resource.name));
        }
    }
    if resources
        .values()
        .any(|resource| matches!(resource, ResourceDefinition::State { .. }))
    {
        let state_api_id = ApiId::new("trellis.state@v1");
        if let Some(state_api) = scope
            .dependencies
            .get(&PackageId::new("trellis"))
            .and_then(|package| package.apis.get(&state_api_id))
        {
            let selected =
                uses.entry(state_api_id.clone())
                    .or_insert_with(|| InteractionSelection {
                        api: state_api_id,
                        actions: BTreeSet::new(),
                        optional_capabilities: BTreeSet::new(),
                    });
            for name in ["Get", "Put", "Delete"] {
                let action = ActionId {
                    kind: ActionKind::Rpc,
                    name: name.to_owned(),
                };
                if !state_api.actions.contains_key(&action) {
                    return Err(miette!("Trellis State API is missing RPC '{name}'"));
                }
                selected.actions.insert(ActionSelection {
                    action,
                    direction: InteractionDirection::Call,
                });
            }
        }
    }
    let mut companions = Vec::new();
    let companion = if let Some(child) = &raw.companion {
        if kind != ParticipantKind::Device {
            return Err(miette!("only a device may contain a companion"));
        }
        let (child_definition, descendants) =
            resolve_participant(child, Some(&lexical), source, scope, root)?;
        let reference = CompanionDefinition {
            participant: child_definition.identity.clone(),
            optional: child.optional,
        };
        companions.push(child_definition);
        companions.extend(descendants);
        Some(reference)
    } else {
        None
    };
    Ok((
        ParticipantDefinition {
            identity,
            name: raw.name.clone(),
            kind,
            implements,
            uses,
            resources,
            companion,
        },
        companions,
    ))
}

fn resolve_resource(
    raw: &ast::Resource,
    participant: ParticipantKind,
    source: usize,
    scope: &Scope<'_>,
    root: &SemanticPackage,
) -> miette::Result<ResourceDefinition> {
    let docs = Documentation {
        title: text(raw, "title")?.to_owned(),
        description: text(raw, "description")?.to_owned(),
    };
    if docs.title.is_empty() || docs.description.is_empty() {
        return Err(miette!(
            "resource '{}' requires nonempty title and description",
            raw.name
        ));
    }
    let type_ref = |member: &str| -> miette::Result<TypeRef> {
        resolve_type_ref(name(raw, member)?, source, scope)
    };
    let optional_type = |member: &str| -> miette::Result<Option<TypeRef>> {
        raw.members
            .get(member)
            .map(|_| type_ref(member))
            .transpose()
    };
    let version = integer_or(raw, "version", 1)?;
    let version =
        u32::try_from(version).map_err(|_| miette!("representation version exceeds u32"))?;
    if version == 0 {
        return Err(miette!("representation version must be positive"));
    }
    let accepts = accepts(raw, source, scope, version)?;
    Ok(match raw.kind.as_str() {
        "state" if !matches!(participant, ParticipantKind::Service) => ResourceDefinition::State {
            optional: {
                reject_except(
                    raw,
                    &["title", "description", "schema", "version", "accepts"],
                )?;
                raw.optional
            },
            docs,
            schema: type_ref("schema")?,
            version,
            accepts,
        },
        "state" => return Err(miette!("State resources are illegal for services")),
        "kv" => ResourceDefinition::Kv {
            optional: {
                reject_except(
                    raw,
                    &[
                        "title",
                        "description",
                        "schema",
                        "version",
                        "accepts",
                        "history",
                        "ttl",
                        "desired_max_value",
                    ],
                )?;
                raw.optional
            },
            docs,
            schema: type_ref("schema")?,
            version,
            accepts,
            history: {
                let history = integer_or(raw, "history", 1)?;
                if history == 0 {
                    return Err(miette!("KV history must be positive"));
                }
                history
            },
            ttl_ms: duration_or(raw, "ttl", 0)?,
            desired_max_value: capacity(raw, "desired_max_value")?,
        },
        "store" => {
            reject_except(
                raw,
                &[
                    "title",
                    "description",
                    "ttl",
                    "desired_max_object",
                    "desired_max_total",
                ],
            )?;
            ResourceDefinition::Store {
                optional: raw.optional,
                docs,
                ttl_ms: duration_or(raw, "ttl", 0)?,
                desired_max_object: capacity(raw, "desired_max_object")?,
                desired_max_total: capacity(raw, "desired_max_total")?,
            }
        }
        "job" => {
            reject_except(
                raw,
                &[
                    "title",
                    "description",
                    "payload",
                    "result",
                    "update",
                    "deadline",
                    "retry",
                    "key_concurrency",
                ],
            )?;
            let key_concurrency = match raw.members.get("key_concurrency") {
                Some(ResourceValue::KeyConcurrency { path, policy }) => {
                    let payload = type_ref("payload")?;
                    validate_scalar_path(&payload, path, scope.dependencies, root)?;
                    Some(KeyConcurrency {
                        path: path.clone(),
                        policy: match policy.as_str() {
                            "queue" => KeyConcurrencyPolicy::Queue,
                            "reject" => KeyConcurrencyPolicy::Reject,
                            "supersede" => KeyConcurrencyPolicy::Supersede,
                            _ => return Err(miette!("unknown key concurrency policy '{policy}'")),
                        },
                    })
                }
                None => None,
                _ => return Err(miette!("invalid key_concurrency")),
            };
            ResourceDefinition::Job {
                optional: raw.optional,
                docs,
                payload: type_ref("payload")?,
                result: optional_type("result")?,
                update: optional_type("update")?,
                deadline_ms: match raw.members.get("deadline") {
                    Some(ResourceValue::Duration(value)) if *value > 0 => Some(*value),
                    Some(ResourceValue::Duration(_)) => {
                        return Err(miette!("job deadline must be positive"));
                    }
                    None => None,
                    _ => return Err(miette!("invalid deadline")),
                },
                retry: retry(raw)?,
                key_concurrency,
            }
        }
        "consumer" => {
            reject_except(
                raw,
                &[
                    "title",
                    "description",
                    "events",
                    "concurrency",
                    "replay",
                    "retry",
                ],
            )?;
            let events: BTreeSet<(ApiId, String)> = names(raw, "events")?
                .iter()
                .map(|event| resolve_event(event, source, scope, root))
                .collect::<miette::Result<_>>()?;
            if events.is_empty() {
                return Err(miette!("Consumer requires one or more exact events"));
            }
            if events.len() != names(raw, "events")?.len() {
                return Err(miette!("Consumer event selections must be distinct"));
            }
            let concurrency = u32::try_from(integer_or(raw, "concurrency", 1)?)
                .map_err(|_| miette!("consumer concurrency exceeds u32"))?;
            if concurrency == 0 {
                return Err(miette!("consumer concurrency must be positive"));
            }
            let replay = match raw.members.get("replay") {
                None => Replay::New,
                Some(ResourceValue::Name(value)) if value == "new" => Replay::New,
                Some(ResourceValue::Name(value)) if value == "all" => Replay::All,
                _ => return Err(miette!("consumer replay must be new or all")),
            };
            ResourceDefinition::Consumer {
                optional: raw.optional,
                docs,
                events,
                concurrency,
                replay,
                retry: retry(raw)?,
            }
        }
        _ => return Err(miette!("unknown resource kind '{}'", raw.kind)),
    })
}

fn text<'a>(raw: &'a ast::Resource, name: &str) -> miette::Result<&'a str> {
    match raw.members.get(name) {
        Some(ResourceValue::Text(value)) => Ok(value),
        _ => Err(miette!("resource '{}' requires '{name}'", raw.name)),
    }
}
fn name<'a>(raw: &'a ast::Resource, name: &str) -> miette::Result<&'a str> {
    match raw.members.get(name) {
        Some(ResourceValue::Name(value)) => Ok(value),
        _ => Err(miette!("resource '{}' requires '{name}'", raw.name)),
    }
}
fn names<'a>(raw: &'a ast::Resource, name: &str) -> miette::Result<&'a [String]> {
    match raw.members.get(name) {
        Some(ResourceValue::Names(value)) => Ok(value),
        _ => Err(miette!("resource '{}' requires '{name}'", raw.name)),
    }
}
fn integer_or(raw: &ast::Resource, name: &str, default: u64) -> miette::Result<u64> {
    match raw.members.get(name) {
        Some(ResourceValue::Integer(value)) => Ok(*value),
        None => Ok(default),
        _ => Err(miette!("invalid integer member '{name}'")),
    }
}
fn duration_or(raw: &ast::Resource, name: &str, default: u64) -> miette::Result<u64> {
    match raw.members.get(name) {
        Some(ResourceValue::Duration(value)) => Ok(*value),
        None => Ok(default),
        _ => Err(miette!("invalid duration member '{name}'")),
    }
}
fn capacity(raw: &ast::Resource, name: &str) -> miette::Result<Option<u64>> {
    match raw.members.get(name) {
        Some(ResourceValue::Capacity(value)) => Ok(Some(*value)),
        None => Ok(None),
        _ => Err(miette!("invalid capacity member '{name}'")),
    }
}
fn retry(raw: &ast::Resource) -> miette::Result<Option<RetryPolicy>> {
    match raw.members.get("retry") {
        Some(ResourceValue::Retry {
            attempts,
            backoff_ms,
        }) if *attempts > 0
            && backoff_ms.len() == *attempts as usize - 1
            && backoff_ms.iter().all(|value| *value > 0) =>
        {
            Ok(Some(RetryPolicy {
                attempts: *attempts,
                backoff_ms: backoff_ms.clone(),
            }))
        }
        Some(ResourceValue::Retry { .. }) => Err(miette!(
            "retry requires positive attempts and exactly attempts-1 positive backoffs"
        )),
        None => Ok(None),
        _ => Err(miette!("invalid retry")),
    }
}
fn reject_except(raw: &ast::Resource, allowed: &[&str]) -> miette::Result<()> {
    if let Some(name) = raw
        .members
        .keys()
        .find(|name| !allowed.contains(&name.as_str()))
    {
        return Err(miette!("{} resource does not support '{name}'", raw.kind));
    }
    Ok(())
}

fn accepts(
    raw: &ast::Resource,
    source: usize,
    scope: &Scope<'_>,
    current: u32,
) -> miette::Result<Vec<HistoricRepresentation>> {
    let values = match raw.members.get("accepts") {
        Some(ResourceValue::Accepts(values)) => values,
        None => return Ok(Vec::new()),
        _ => return Err(miette!("invalid accepts member")),
    };
    let mut seen = BTreeSet::new();
    values.iter().map(|(version, ty)| {
        if *version == 0 || *version == current || !seen.insert(*version) { return Err(miette!("accepted representation versions must be distinct, positive, and differ from current")); }
        Ok(HistoricRepresentation { version: *version, ty: resolve_type_ref(ty, source, scope)? })
    }).collect()
}

fn resolve_event(
    value: &str,
    source: usize,
    scope: &Scope<'_>,
    root: &SemanticPackage,
) -> miette::Result<(ApiId, String)> {
    let Some((api_name, action)) = value.split_once('.') else {
        return Err(miette!(
            "consumer event '{value}' must identify an imported API and exact event"
        ));
    };
    let (package, imported) = resolve_name(api_name, source, scope)?;
    let package = if package == *scope.package {
        root
    } else {
        &scope.dependencies[&package]
    };
    let api = package
        .apis
        .values()
        .find(|api| api.name == imported)
        .ok_or_else(|| miette!("unknown API '{imported}'"))?;
    let id = ActionId {
        kind: ActionKind::Event,
        name: action.to_owned(),
    };
    if !api.actions.contains_key(&id) {
        return Err(miette!("unknown event '{value}'"));
    }
    Ok((api.identity.clone(), action.to_owned()))
}

fn validate_scalar_path(
    reference: &TypeRef,
    path: &[String],
    dependencies: &BTreeMap<PackageId, SemanticPackage>,
    root: &SemanticPackage,
) -> miette::Result<()> {
    if path.is_empty() {
        return Err(miette!("typed field path must not be empty"));
    }
    scalar_path_in_semantic(
        root,
        dependencies,
        &TypeExpression::Named(reference.clone()),
        path,
    )
}

fn validate_cursor_usage(
    package: &SemanticPackage,
    dependencies: &BTreeMap<PackageId, SemanticPackage>,
) -> miette::Result<()> {
    let mut paginated_inputs = BTreeSet::new();
    let mut paginated_outputs = BTreeSet::new();
    for action in package.apis.values().flat_map(|api| api.actions.values()) {
        if let ActionDefinition::Rpc {
            input,
            output,
            pagination: Some(_),
            ..
        } = action
        {
            paginated_inputs.insert(terminal_reference(input, package, dependencies)?);
            paginated_outputs.insert(terminal_reference(output, package, dependencies)?);
        }
    }
    for (id, definition) in &package.types {
        match definition {
            TypeDefinition::Model(fields) => {
                for (name, field) in fields {
                    let valid_page = name == "page"
                        && field.optional
                        && matches!(field.ty, TypeExpression::CursorQuery)
                        && paginated_inputs.contains(&TypeRef {
                            package: package.identity.clone(),
                            id: id.clone(),
                        });
                    if cursor_flags_expr(&field.ty, package, dependencies, &mut BTreeSet::new())
                        != (false, false)
                        && !valid_page
                    {
                        return Err(miette!(
                            "pagination built-ins are invalid at model field '{id}.{name}'"
                        ));
                    }
                }
            }
            TypeDefinition::Alias(TypeExpression::CursorPage(member))
                if paginated_outputs.contains(&TypeRef {
                    package: package.identity.clone(),
                    id: id.clone(),
                }) =>
            {
                if cursor_flags_expr(member, package, dependencies, &mut BTreeSet::new())
                    != (false, false)
                {
                    return Err(miette!(
                        "CursorPage element type cannot contain pagination built-ins"
                    ));
                }
            }
            TypeDefinition::Alias(value)
                if cursor_flags_expr(value, package, dependencies, &mut BTreeSet::new())
                    != (false, false) =>
            {
                return Err(miette!(
                    "CursorQuery and CursorPage require their exact named pagination shapes"
                ));
            }
            TypeDefinition::Alias(_) | TypeDefinition::Enum(_) => {}
        }
    }
    for api in package.apis.values() {
        for action in api.actions.values() {
            let check = |reference: &TypeRef, allowed: (bool, bool)| -> miette::Result<()> {
                let definition = referenced_definition(reference, package, dependencies)?;
                let actual = cursor_flags_definition(
                    definition,
                    package,
                    dependencies,
                    &mut BTreeSet::new(),
                );
                if actual != allowed && actual != (false, false) {
                    return Err(miette!(
                        "pagination built-in used outside its paginated RPC position"
                    ));
                }
                Ok(())
            };
            match action {
                ActionDefinition::Rpc {
                    input,
                    output,
                    pagination: Some(_),
                    ..
                } => {
                    check(input, (true, false))?;
                    check(output, (false, true))?;
                }
                ActionDefinition::Rpc { input, output, .. } => {
                    check(input, (false, false))?;
                    check(output, (false, false))?;
                }
                ActionDefinition::Operation {
                    input,
                    output,
                    update,
                    signals,
                    ..
                } => {
                    check(input, (false, false))?;
                    check(output, (false, false))?;
                    if let Some(update) = update {
                        check(update, (false, false))?;
                    }
                    for signal in signals.values() {
                        check(signal, (false, false))?;
                    }
                }
                ActionDefinition::Event { payload, .. } => check(payload, (false, false))?,
                ActionDefinition::Feed { input, event } => {
                    check(input, (false, false))?;
                    check(event, (false, false))?;
                }
            }
        }
        for error in api.errors.values().flatten() {
            let definition = referenced_definition(error, package, dependencies)?;
            if cursor_flags_definition(definition, package, dependencies, &mut BTreeSet::new())
                != (false, false)
            {
                return Err(miette!("pagination built-in used in an error payload"));
            }
        }
    }
    Ok(())
}

fn cursor_flags_definition(
    definition: &TypeDefinition,
    root: &SemanticPackage,
    dependencies: &BTreeMap<PackageId, SemanticPackage>,
    visited: &mut BTreeSet<TypeRef>,
) -> (bool, bool) {
    match definition {
        TypeDefinition::Model(fields) => fields.values().fold((false, false), |flags, field| {
            combine_flags(
                flags,
                cursor_flags_expr(&field.ty, root, dependencies, visited),
            )
        }),
        TypeDefinition::Alias(value) => cursor_flags_expr(value, root, dependencies, visited),
        TypeDefinition::Enum(_) => (false, false),
    }
}

fn cursor_flags_expr(
    value: &TypeExpression,
    root: &SemanticPackage,
    dependencies: &BTreeMap<PackageId, SemanticPackage>,
    visited: &mut BTreeSet<TypeRef>,
) -> (bool, bool) {
    match value {
        TypeExpression::CursorQuery => (true, false),
        TypeExpression::CursorPage(member) => combine_flags(
            (false, true),
            cursor_flags_expr(member, root, dependencies, visited),
        ),
        TypeExpression::Named(reference) if visited.insert(reference.clone()) => {
            package_for(root, dependencies, &reference.package)
                .ok()
                .and_then(|package| package.types.get(&reference.id))
                .map_or((false, false), |definition| {
                    cursor_flags_definition(definition, root, dependencies, visited)
                })
        }
        TypeExpression::List(member, _)
        | TypeExpression::Map(member)
        | TypeExpression::Nullable(member) => {
            cursor_flags_expr(member, root, dependencies, visited)
        }
        TypeExpression::Named(_) | TypeExpression::Primitive(_, _) => (false, false),
    }
}

fn combine_flags(left: (bool, bool), right: (bool, bool)) -> (bool, bool) {
    (left.0 || right.0, left.1 || right.1)
}

fn scalar_path_in_semantic(
    root: &SemanticPackage,
    dependencies: &BTreeMap<PackageId, SemanticPackage>,
    ty: &TypeExpression,
    path: &[String],
) -> miette::Result<()> {
    let mut ty = ty;
    let mut aliases = BTreeSet::new();
    for component in path {
        loop {
            let TypeExpression::Named(reference) = ty else {
                return Err(miette!(
                    "typed path component '{component}' does not select a model"
                ));
            };
            if !aliases.insert(reference.clone()) {
                return Err(miette!("type alias cycle in typed field path"));
            }
            let package = package_for(root, dependencies, &reference.package)?;
            match &package.types[&reference.id] {
                TypeDefinition::Alias(value) => ty = value,
                TypeDefinition::Model(fields) => {
                    let field = fields
                        .get(component)
                        .ok_or_else(|| miette!("unknown typed path component '{component}'"))?;
                    if field.optional {
                        return Err(miette!("typed path component '{component}' is optional"));
                    }
                    ty = &field.ty;
                    break;
                }
                TypeDefinition::Enum(_) => {
                    return Err(miette!("enum cannot contain typed path components"));
                }
            }
        }
    }
    scalar_leaf(root, dependencies, ty)
}

fn scalar_leaf(
    root: &SemanticPackage,
    dependencies: &BTreeMap<PackageId, SemanticPackage>,
    ty: &TypeExpression,
) -> miette::Result<()> {
    match ty {
        TypeExpression::Primitive(primitive, _)
            if !matches!(primitive, Primitive::Bytes | Primitive::Number) =>
        {
            Ok(())
        }
        TypeExpression::Named(reference) => {
            match &package_for(root, dependencies, &reference.package)?.types[&reference.id] {
                TypeDefinition::Alias(value) => scalar_leaf(root, dependencies, value),
                TypeDefinition::Enum(_) => Ok(()),
                _ => Err(miette!("typed path leaf must be a scalar")),
            }
        }
        _ => Err(miette!("typed path leaf must be a scalar")),
    }
}

fn package_for<'a>(
    root: &'a SemanticPackage,
    dependencies: &'a BTreeMap<PackageId, SemanticPackage>,
    id: &PackageId,
) -> miette::Result<&'a SemanticPackage> {
    if id == &root.identity {
        Ok(root)
    } else {
        dependencies
            .get(id)
            .ok_or_else(|| miette!("package '{id}' is absent from the resolved closure"))
    }
}

fn validate_type_graph(package: &SemanticPackage) -> miette::Result<()> {
    fn visit(
        id: &TypeId,
        package: &SemanticPackage,
        aliases: &mut BTreeSet<TypeId>,
        complete: &mut BTreeSet<TypeId>,
    ) -> miette::Result<()> {
        if complete.contains(id) {
            return Ok(());
        }
        if !aliases.insert(id.clone()) {
            return Err(miette!("unproductive named alias cycle involving '{id}'"));
        }
        if let TypeDefinition::Alias(TypeExpression::Named(reference)) = &package.types[id] {
            if reference.package == package.identity {
                visit(&reference.id, package, aliases, complete)?;
            }
        }
        aliases.remove(id);
        complete.insert(id.clone());
        Ok(())
    }
    let mut complete = BTreeSet::new();
    for id in package.types.keys() {
        visit(id, package, &mut BTreeSet::new(), &mut complete)?;
    }
    Ok(())
}

fn mark_recursive_model_fields(package: &mut SemanticPackage) {
    fn direct_model(
        expression: &TypeExpression,
        package: &SemanticPackage,
        visited: &mut BTreeSet<TypeId>,
    ) -> Option<TypeId> {
        match expression {
            TypeExpression::Named(reference)
                if reference.package == package.identity
                    && visited.insert(reference.id.clone()) =>
            {
                match &package.types[&reference.id] {
                    TypeDefinition::Model(_) => Some(reference.id.clone()),
                    TypeDefinition::Alias(value) => direct_model(value, package, visited),
                    TypeDefinition::Enum(_) => None,
                }
            }
            TypeExpression::Nullable(value) => direct_model(value, package, visited),
            _ => None,
        }
    }

    fn reaches(
        current: &TypeId,
        target: &TypeId,
        edges: &BTreeMap<TypeId, BTreeSet<TypeId>>,
        visited: &mut BTreeSet<TypeId>,
    ) -> bool {
        current == target
            || visited.insert(current.clone())
                && edges[current]
                    .iter()
                    .any(|next| reaches(next, target, edges, visited))
    }

    let mut fields = Vec::new();
    let mut edges = package
        .types
        .iter()
        .filter(|(_, definition)| matches!(definition, TypeDefinition::Model(_)))
        .map(|(id, _)| (id.clone(), BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    for (model, definition) in &package.types {
        let TypeDefinition::Model(model_fields) = definition else {
            continue;
        };
        for (name, field) in model_fields {
            if let Some(target) = direct_model(&field.ty, package, &mut BTreeSet::new()) {
                edges
                    .get_mut(model)
                    .expect("model edge owner exists")
                    .insert(target.clone());
                fields.push((model.clone(), name.clone(), target));
            }
        }
    }
    for (model, name, target) in fields {
        if reaches(&target, &model, &edges, &mut BTreeSet::new()) {
            let TypeDefinition::Model(fields) = package
                .types
                .get_mut(&model)
                .expect("model edge owner exists")
            else {
                unreachable!()
            };
            fields.get_mut(&name).expect("model field exists").recursive = true;
        }
    }
}

fn derive_participant_needs(
    graph: &PackageGraph,
    participant_id: &ParticipantId,
) -> miette::Result<ParticipantNeeds> {
    let participant = graph
        .packages
        .values()
        .find_map(|package| package.participants.get(participant_id))
        .ok_or_else(|| miette!("participant '{participant_id}' is absent from graph"))?;
    let mut required = Vec::new();
    let mut optional = BTreeMap::<String, Vec<PermissionAtom>>::new();
    let mut required_capabilities = BTreeSet::new();
    for (api_id, selection) in &participant.uses {
        let api = graph
            .api(api_id)
            .ok_or_else(|| miette!("selected API '{api_id}' is absent from graph"))?
            .definition;
        for selected in &selection.actions {
            let atoms = selected_permission_atoms(api_id, selected, api)?;
            let covering = api
                .capabilities
                .iter()
                .filter(|(_, capability)| capability.allows.contains(selected))
                .collect::<Vec<_>>();
            let mut required_action = covering.is_empty();
            for (capability_id, capability) in covering {
                if selection.optional_capabilities.contains(capability_id) {
                    optional
                        .entry(capability_id.as_str().to_owned())
                        .or_default()
                        .extend(atoms.iter().cloned());
                } else {
                    required_action = true;
                    if !capability.public {
                        required_capabilities.insert(capability_id.clone());
                    }
                }
            }
            if required_action {
                required.extend(atoms);
            }
        }
    }
    for (name, resource) in &participant.resources {
        let atoms = resource_permission_atoms(participant_id, name, resource)?;
        if resource.optional() {
            optional
                .entry(format!("resource.{name}"))
                .or_default()
                .extend(atoms);
        } else {
            required.extend(atoms);
        }
    }
    let required_grants = GrantSet::new(required);
    let optional_grants = optional
        .into_iter()
        .map(|(key, permissions)| (key, GrantSet::new(permissions)))
        .collect::<BTreeMap<_, _>>();
    let mut digest_input = String::new();
    let mut append = |value: &str| {
        use std::fmt::Write as _;
        write!(digest_input, "{}:{value}", value.len()).expect("string writes cannot fail");
    };
    append(participant_id.as_str());
    append(match participant.kind {
        ParticipantKind::Service => "service",
        ParticipantKind::Device => "device",
        ParticipantKind::App => "app",
        ParticipantKind::Agent => "agent",
    });
    if let Some(companion) = &participant.companion {
        append(companion.participant.as_str());
        append(if companion.optional {
            "optional"
        } else {
            "required"
        });
    }
    for api in &participant.implements {
        append(&crate::api_digest(graph, api)?);
    }
    for selection in participant.uses.values() {
        append(&crate::selected_surface_digest(graph, selection)?);
    }
    for (name, resource) in &participant.resources {
        append(name.as_str());
        append(&resource_needs_json(graph, resource)?);
    }
    append(
        &required_grants
            .digest()
            .map_err(|error| miette!(error.to_string()))?,
    );
    for capability in &required_capabilities {
        append(capability.as_str());
    }
    for (key, grants) in &optional_grants {
        append(key);
        append(
            &grants
                .digest()
                .map_err(|error| miette!(error.to_string()))?,
        );
    }
    Ok(ParticipantNeeds {
        digest: trellis_protocol::sha256_base64url(&digest_input),
        required_grants,
        optional_grants,
        required_capabilities,
    })
}

fn resource_needs_json(
    graph: &PackageGraph,
    resource: &ResourceDefinition,
) -> miette::Result<String> {
    let schema = |value: &TypeRef| crate::json_schema(graph, value);
    let representations = |current: &TypeRef, version: u32, accepts: &[HistoricRepresentation]| {
        Ok::<_, miette::Report>(serde_json::json!({
            "schema": schema(current)?,
            "version": version,
            "accepts": accepts
                .iter()
                .map(|value| Ok(serde_json::json!({
                    "version": value.version,
                    "schema": schema(&value.ty)?,
                })))
                .collect::<miette::Result<Vec<_>>>()?,
        }))
    };
    let retry = |value: &Option<RetryPolicy>| {
        value.as_ref().map(
            |value| serde_json::json!({"attempts": value.attempts, "backoffMs": value.backoff_ms}),
        )
    };
    let value = match resource {
        ResourceDefinition::State {
            optional,
            schema: current,
            version,
            accepts,
            ..
        } => serde_json::json!({
            "kind": "state",
            "optional": optional,
            "representation": representations(current, *version, accepts)?,
        }),
        ResourceDefinition::Kv {
            optional,
            schema: current,
            version,
            accepts,
            history,
            ttl_ms,
            desired_max_value,
            ..
        } => serde_json::json!({
            "kind": "kv",
            "optional": optional,
            "representation": representations(current, *version, accepts)?,
            "history": history,
            "ttlMs": ttl_ms,
            "desiredMaxValue": desired_max_value,
        }),
        ResourceDefinition::Store {
            optional,
            ttl_ms,
            desired_max_object,
            desired_max_total,
            ..
        } => serde_json::json!({
            "kind": "store",
            "optional": optional,
            "ttlMs": ttl_ms,
            "desiredMaxObject": desired_max_object,
            "desiredMaxTotal": desired_max_total,
        }),
        ResourceDefinition::Job {
            optional,
            payload,
            result,
            update,
            deadline_ms,
            retry: retry_value,
            key_concurrency,
            ..
        } => serde_json::json!({
            "kind": "job",
            "optional": optional,
            "payload": schema(payload)?,
            "result": result.as_ref().map(schema).transpose()?,
            "update": update.as_ref().map(schema).transpose()?,
            "deadlineMs": deadline_ms,
            "retry": retry(retry_value),
            "keyConcurrency": key_concurrency.as_ref().map(|value| serde_json::json!({
                "path": value.path,
                "policy": match value.policy {
                    KeyConcurrencyPolicy::Queue => "queue",
                    KeyConcurrencyPolicy::Reject => "reject",
                    KeyConcurrencyPolicy::Supersede => "supersede",
                },
            })),
        }),
        ResourceDefinition::Consumer {
            optional,
            events,
            concurrency,
            replay,
            retry: retry_value,
            ..
        } => serde_json::json!({
            "kind": "consumer",
            "optional": optional,
            "events": events.iter().map(|(api, event)| format!("{api}.{event}")).collect::<Vec<_>>(),
            "concurrency": concurrency,
            "replay": match replay { Replay::New => "new", Replay::All => "all" },
            "retry": retry(retry_value),
        }),
    };
    trellis_protocol::canonicalize_json(&value).map_err(|error| miette!(error.to_string()))
}

/// Derive the complete exact permission atoms for one selected interaction.
#[doc(hidden)]
pub fn selected_permission_atoms(
    api_id: &ApiId,
    selected: &ActionSelection,
    api: &ApiDefinition,
) -> miette::Result<Vec<PermissionAtom>> {
    let target = |surface, action| -> miette::Result<PermissionAtom> {
        let target = PermissionTarget::api_surface(api_id.as_str(), surface, &selected.action.name)
            .map_err(|error| miette!(error.to_string()))?;
        PermissionAtom::new(target, action).map_err(|error| miette!(error.to_string()))
    };
    Ok(match (selected.action.kind, selected.direction) {
        (ActionKind::Rpc, InteractionDirection::Call) => {
            vec![target(ApiSurfaceKind::Rpc, PermissionAction::Call)?]
        }
        (ActionKind::Operation, InteractionDirection::Invoke) => {
            let mut atoms = vec![
                target(ApiSurfaceKind::Operation, PermissionAction::Invoke)?,
                target(ApiSurfaceKind::Operation, PermissionAction::Observe)?,
                target(ApiSurfaceKind::Operation, PermissionAction::Cancel)?,
            ];
            let ActionDefinition::Operation { signals, .. } = &api.actions[&selected.action] else {
                unreachable!()
            };
            for signal in signals.keys() {
                atoms.push(
                    PermissionAtom::new(
                        PermissionTarget::operation_signal(
                            api_id.as_str(),
                            &selected.action.name,
                            signal,
                        )
                        .map_err(|error| miette!(error.to_string()))?,
                        PermissionAction::Control,
                    )
                    .map_err(|error| miette!(error.to_string()))?,
                );
            }
            atoms
        }
        (ActionKind::Event, InteractionDirection::Publish) => {
            vec![target(ApiSurfaceKind::Event, PermissionAction::Publish)?]
        }
        (ActionKind::Event, InteractionDirection::Subscribe) => {
            vec![target(ApiSurfaceKind::Event, PermissionAction::Subscribe)?]
        }
        (ActionKind::Feed, InteractionDirection::Subscribe) => {
            vec![target(ApiSurfaceKind::Feed, PermissionAction::Subscribe)?]
        }
        _ => return Err(miette!("selected action direction is invalid")),
    })
}

fn resource_permission_atoms(
    participant: &ParticipantId,
    name: &ResourceName,
    resource: &ResourceDefinition,
) -> miette::Result<Vec<PermissionAtom>> {
    let (kind, actions): (_, &[PermissionAction]) = match resource {
        ResourceDefinition::State { .. } => (
            ParticipantResourceKind::State,
            &[
                PermissionAction::Read,
                PermissionAction::Write,
                PermissionAction::Delete,
            ],
        ),
        ResourceDefinition::Kv { .. } => (
            ParticipantResourceKind::Kv,
            &[
                PermissionAction::Read,
                PermissionAction::Write,
                PermissionAction::Delete,
            ],
        ),
        ResourceDefinition::Store { .. } => (
            ParticipantResourceKind::Store,
            &[
                PermissionAction::Read,
                PermissionAction::Write,
                PermissionAction::Delete,
            ],
        ),
        ResourceDefinition::Job { .. } => (
            ParticipantResourceKind::JobQueue,
            &[PermissionAction::Submit, PermissionAction::Process],
        ),
        ResourceDefinition::Consumer { .. } => (
            ParticipantResourceKind::EventConsumer,
            &[
                PermissionAction::Read,
                PermissionAction::Consume,
                PermissionAction::Control,
            ],
        ),
    };
    actions
        .iter()
        .map(|action| {
            PermissionAtom::new(
                PermissionTarget::participant_resource(participant.as_str(), kind, name.as_str())
                    .map_err(|error| miette!(error.to_string()))?,
                *action,
            )
            .map_err(|error| miette!(error.to_string()))
        })
        .collect()
}

fn exported_types(package: &SemanticPackage) -> BTreeSet<TypeId> {
    fn add(reference: &TypeRef, package: &SemanticPackage, result: &mut BTreeSet<TypeId>) {
        if reference.package != package.identity || !result.insert(reference.id.clone()) {
            return;
        }
        match &package.types[&reference.id] {
            TypeDefinition::Model(fields) => fields
                .values()
                .for_each(|field| add_expr(&field.ty, package, result)),
            TypeDefinition::Alias(value) => add_expr(value, package, result),
            TypeDefinition::Enum(_) => {}
        }
    }
    fn add_expr(value: &TypeExpression, package: &SemanticPackage, result: &mut BTreeSet<TypeId>) {
        match value {
            TypeExpression::Named(reference) => add(reference, package, result),
            TypeExpression::List(value, _)
            | TypeExpression::Map(value)
            | TypeExpression::Nullable(value)
            | TypeExpression::CursorPage(value) => add_expr(value, package, result),
            _ => {}
        }
    }
    let mut result = BTreeSet::new();
    for api in package.apis.values() {
        for payload in api.errors.values().flatten() {
            add(payload, package, &mut result);
        }
        for action in api.actions.values() {
            match action {
                ActionDefinition::Rpc { input, output, .. } => {
                    add(input, package, &mut result);
                    add(output, package, &mut result);
                }
                ActionDefinition::Operation {
                    input,
                    output,
                    update,
                    signals,
                    ..
                } => {
                    add(input, package, &mut result);
                    add(output, package, &mut result);
                    if let Some(value) = update {
                        add(value, package, &mut result);
                    }
                    signals
                        .values()
                        .for_each(|value| add(value, package, &mut result));
                }
                ActionDefinition::Event { payload, .. } => add(payload, package, &mut result),
                ActionDefinition::Feed { input, event } => {
                    add(input, package, &mut result);
                    add(event, package, &mut result);
                }
            }
        }
    }
    result
}

fn record_span(
    package: &mut SemanticPackage,
    parsed: &[ParsedSource],
    key: String,
    declaration: &Spanned<Declaration>,
) {
    package.sources.insert(
        key,
        SourceSpan {
            path: parsed[declaration.source].path.clone(),
            range: declaration.span.clone(),
        },
    );
}
fn record_inner_span(
    package: &mut SemanticPackage,
    parsed: &[ParsedSource],
    source: usize,
    key: String,
    range: std::ops::Range<usize>,
) {
    package.sources.insert(
        key,
        SourceSpan {
            path: parsed[source].path.clone(),
            range,
        },
    );
}
fn at(
    source: &ParsedSource,
    declaration: &Spanned<Declaration>,
    message: impl Into<String>,
) -> miette::Report {
    crate::parser::diagnostic(
        &SourceUnit {
            alias: source.alias.clone(),
            path: source.path.clone(),
            source: source.text.clone(),
        },
        declaration.span.clone(),
        message,
    )
}
