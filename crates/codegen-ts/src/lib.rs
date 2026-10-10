//! Browser-safe TypeScript generation from the native Trellis package graph.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use oxc_allocator::Allocator;
use oxc_codegen::Codegen;
use oxc_isolated_declarations::{IsolatedDeclarations, IsolatedDeclarationsOptions};
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;
use oxc_transformer::{TransformOptions, Transformer};
use trellis_idl::{
    ActionDefinition, ActionId, ActionKind, AppKind, AppRequestDefinition, CanonicalMode,
    InteractionDirection, PackageGraph, ParticipantDefinition, ParticipantKind, Primitive,
    ResourceDefinition, SemanticPackage, TypeDefinition, TypeExpression, TypeRef,
};

/// Errors returned while generating a TypeScript package.
#[derive(thiserror::Error, Debug)]
pub enum CodegenTsError {
    /// A native graph projection failed.
    #[error("IDL projection error: {0}")]
    Idl(String),
    /// A generated file could not be read or written.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Package metadata could not be serialized.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    /// Oxc rejected generated TypeScript.
    #[error("invalid generated TypeScript in {path}: {message}")]
    InvalidTypeScript { path: PathBuf, message: String },
    /// Two authored names produce the same public TypeScript export.
    #[error("generated TypeScript export name collision: {0}")]
    ExportNameCollision(String),
    /// A caller supplied a source path outside the generated package.
    #[error("generated TypeScript output path must stay within the package: {0}")]
    InvalidOutputPath(PathBuf),
    /// A graph reference could not be resolved.
    #[error("missing generated reference: {0}")]
    MissingReference(String),
}

/// One generated TypeScript source file before Oxc emission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedTsSource {
    /// Package-relative output path.
    pub path: PathBuf,
    /// TypeScript source text.
    pub contents: String,
}

/// Render one npm ESM package into an empty caller-owned staging directory.
///
/// All sources are validated and emitted before the first file is written.
pub fn generate_ts_package(
    graph: &PackageGraph,
    out_dir: &Path,
    name: &str,
) -> Result<(), CodegenTsError> {
    let sources = collect_ts_package_sources(graph, name)?;
    let mut emitted = Vec::new();
    for source in sources {
        if source
            .path
            .extension()
            .is_some_and(|extension| extension == "ts")
        {
            emitted.extend(emit_typescript(&source)?);
        } else {
            emitted.push(source);
        }
    }
    write_ts_sdk_sources(out_dir, &emitted)
}

/// Render all package sources without writing files.
pub fn collect_ts_package_sources(
    graph: &PackageGraph,
    name: &str,
) -> Result<Vec<GeneratedTsSource>, CodegenTsError> {
    let root = graph.root_package();
    let public_types = api_reachable_types(graph);
    let participant_types = root
        .participants()
        .iter()
        .map(|(id, participant)| (id, participant_reachable_types(graph, participant)))
        .collect::<BTreeMap<_, _>>();
    validate_names(graph, &public_types, &participant_types)?;
    let package_modules = graph
        .packages()
        .keys()
        .enumerate()
        .map(|(index, id)| (id.as_str().to_owned(), format!("p{index}")))
        .collect::<BTreeMap<_, _>>();
    let api_modules = api_modules(graph);
    let mut sources = vec![GeneratedTsSource {
        path: "package.json".into(),
        contents: serde_json::to_string_pretty(&serde_json::json!({
            "name": name,
            "version": root.version().to_string(),
            "private": true,
            "trellisGenerated": true,
            "description": "Generated Trellis APIs and participants.",
            "type": "module",
            "sideEffects": false,
            "exports": {".": {"types": "./index.d.ts", "import": "./index.js"}},
            "dependencies": {"@oatscenter/trellis": format!("^{}", env!("CARGO_PKG_VERSION"))}
        }))?,
    }];

    for (package_id, package) in graph.packages() {
        sources.push(GeneratedTsSource {
            path: PathBuf::from("types/_internal")
                .join(&package_modules[package_id.as_str()])
                .with_extension("ts"),
            contents: render_types(package, &package_modules),
        });
    }
    sources.push(GeneratedTsSource {
        path: "types/index.ts".into(),
        contents: render_type_exports(&public_types, "./_internal/", &package_modules),
    });

    let mut api_index = String::new();
    for package in graph.packages().values() {
        for api in package.apis().values() {
            let (export, module) = &api_modules[api.identity().as_str()];
            writeln!(
                api_index,
                "export * as {} from \"./{}/mod.ts\";",
                export, module
            )
            .unwrap();
            sources.push(GeneratedTsSource {
                path: PathBuf::from("apis").join(module).join("mod.ts"),
                contents: render_api(graph, api, &package_modules)?,
            });
        }
    }
    if api_index.is_empty() {
        api_index.push_str("export {};\n");
    }
    sources.push(GeneratedTsSource {
        path: "apis/index.ts".into(),
        contents: api_index,
    });

    let mut participant_index = String::new();
    for participant in root.participants().values() {
        writeln!(
            participant_index,
            "export * as {} from \"./{}/mod.ts\";",
            participant.name(),
            participant.name()
        )
        .unwrap();
        render_participant_tree(
            graph,
            root,
            participant,
            PathBuf::from("participants").join(participant.name()),
            &package_modules,
            &participant_types,
            &mut sources,
        )?;
    }
    if participant_index.is_empty() {
        participant_index.push_str("export {};\n");
    }
    sources.push(GeneratedTsSource {
        path: "participants/index.ts".into(),
        contents: participant_index,
    });
    let mut app_index = String::new();
    let mut app_names = BTreeSet::new();
    for request in root.app_requests().values() {
        let name = app_module_name(root, request);
        validate_export_name("application request", &name)?;
        if !app_names.insert(name.to_lowercase()) {
            return Err(CodegenTsError::ExportNameCollision(format!(
                "application request {name}"
            )));
        }
        writeln!(app_index, "export * as {name} from \"./{name}/mod.ts\";").unwrap();
        sources.push(GeneratedTsSource {
            path: PathBuf::from("apps").join(&name).join("mod.ts"),
            contents: render_app_request(graph, request, &package_modules)?,
        });
    }
    if app_index.is_empty() {
        app_index.push_str("export {};\n");
    }
    sources.push(GeneratedTsSource {
        path: "apps/index.ts".into(),
        contents: app_index,
    });
    sources.push(GeneratedTsSource {
        path: "index.ts".into(),
        contents: "export * as apis from \"./apis/index.ts\";\nexport * as participants from \"./participants/index.ts\";\nexport * as apps from \"./apps/index.ts\";\nexport * as types from \"./types/index.ts\";\n".into(),
    });
    Ok(sources)
}

fn validate_names(
    graph: &PackageGraph,
    public_types: &BTreeSet<TypeRef>,
    participant_types: &BTreeMap<&trellis_idl::ParticipantId, BTreeSet<TypeRef>>,
) -> Result<(), CodegenTsError> {
    let api_modules = api_modules(graph);
    let mut paths = BTreeMap::new();
    let mut exports = BTreeMap::new();
    for package in graph.packages().values() {
        for api in package.apis().values() {
            validate_export_name("API", api.name())?;
            let (export, path) = &api_modules[api.identity().as_str()];
            if let Some(previous) = paths.insert(path.to_lowercase(), api.identity().as_str()) {
                return Err(CodegenTsError::ExportNameCollision(format!(
                    "apis/{path}: '{}' and '{}'",
                    previous,
                    api.identity()
                )));
            }
            if let Some(previous) = exports.insert(export.to_lowercase(), api.identity().as_str()) {
                return Err(CodegenTsError::ExportNameCollision(format!(
                    "API export '{export}': '{previous}' and '{}'",
                    api.identity()
                )));
            }
            let mut aliases = BTreeMap::from([
                ("API".to_owned(), "generated API descriptor".to_owned()),
                ("API_DIGEST".to_owned(), "generated API digest".to_owned()),
            ]);
            for (action, definition) in api.actions() {
                let alias = action_type_base(action);
                let mut generated = match definition {
                    ActionDefinition::Rpc { .. } => {
                        vec![format!("{alias}Input"), format!("{alias}Output")]
                    }
                    ActionDefinition::Operation {
                        progress,
                        update,
                        signals,
                        ..
                    } => {
                        let mut names = vec![format!("{alias}Input"), format!("{alias}Output")];
                        if progress.is_some() {
                            names.push(format!("{alias}Progress"));
                        }
                        if update.is_some() || progress.is_some() {
                            names.push(format!("{alias}Update"));
                        }
                        names.extend(
                            signals
                                .keys()
                                .map(|name| format!("{alias}{}Signal", pascal(name))),
                        );
                        names
                    }
                    ActionDefinition::Event { .. } => vec![format!("{alias}Event")],
                    ActionDefinition::Live { .. } => {
                        vec![format!("{alias}Input"), format!("{alias}Event")]
                    }
                };
                for name in generated.drain(..) {
                    insert_generated_name(
                        &mut aliases,
                        &name,
                        &format!("action '{}'", descriptor_name(action)),
                        api.identity().as_str(),
                    )?;
                }
            }
            for error in api.errors().keys() {
                validate_export_name("error", error)?;
                insert_generated_name(
                    &mut aliases,
                    error,
                    &format!("error '{error}'"),
                    api.identity().as_str(),
                )?;
                insert_generated_name(
                    &mut aliases,
                    &format!("{error}Data"),
                    &format!("error '{error}' data"),
                    api.identity().as_str(),
                )?;
            }
        }
    }
    validate_type_export_names("types", public_types)?;
    for (participant, types) in participant_types {
        validate_type_export_names(
            &format!("participant '{}' types", participant.as_str()),
            types,
        )?;
    }
    let root = graph.root_package();
    for participant in root.participants().values() {
        validate_export_name("participant", participant.name())?;
        let mut names = BTreeMap::from([
            ("participant".to_owned(), "generated descriptor".to_owned()),
            ("types".to_owned(), "generated type namespace".to_owned()),
            (
                "Participant".to_owned(),
                "generated participant type".to_owned(),
            ),
            (
                "ResourceDescriptors".to_owned(),
                "generated resource descriptors".to_owned(),
            ),
            (
                "ResourceHandles".to_owned(),
                "generated resource handles".to_owned(),
            ),
            (
                "Availability".to_owned(),
                "generated availability".to_owned(),
            ),
            (
                "ParticipantFacade".to_owned(),
                "generated facade".to_owned(),
            ),
            (
                "PARTICIPANT_DIGEST".to_owned(),
                "generated participant digest".to_owned(),
            ),
            (
                "MigrationOptions".to_owned(),
                "generated migration options".to_owned(),
            ),
            (
                "ConnectionOptions".to_owned(),
                "generated connection options".to_owned(),
            ),
        ]);
        for resource in participant.resources().keys() {
            let exported = pascal(resource.as_str());
            insert_generated_name(
                &mut names,
                &format!("{exported}Resource"),
                &format!("resource '{resource}'"),
                participant.identity().as_str(),
            )?;
            insert_generated_name(
                &mut names,
                &format!("{exported}Handle"),
                &format!("resource '{resource}'"),
                participant.identity().as_str(),
            )?;
        }
    }
    for participant in root.participants().values() {
        if let Some(companion) = participant.companion() {
            let child = root
                .app_requests()
                .get(&companion.request)
                .ok_or_else(|| CodegenTsError::MissingReference(companion.request.to_string()))?;
            if child.name() == participant.name() {
                return Err(CodegenTsError::ExportNameCollision(format!(
                    "participant companion '{}.{}'",
                    participant.name(),
                    child.name()
                )));
            }
            if [
                "types",
                "participant",
                "Participant",
                "ResourceDescriptors",
                "ResourceHandles",
                "Availability",
                "ParticipantFacade",
                "MigrationOptions",
                "ConnectionOptions",
                "PARTICIPANT_DIGEST",
            ]
            .contains(&child.name())
            {
                return Err(CodegenTsError::ExportNameCollision(format!(
                    "participant '{}' companion name '{}' conflicts with a generated export",
                    participant.identity(),
                    child.name()
                )));
            }
        }
    }
    Ok(())
}

fn insert_generated_name(
    names: &mut BTreeMap<String, String>,
    name: &str,
    source: &str,
    scope: &str,
) -> Result<(), CodegenTsError> {
    if let Some(previous) = names.insert(name.to_owned(), source.to_owned()) {
        return Err(CodegenTsError::ExportNameCollision(format!(
            "{scope}: '{name}' is generated by {previous} and {source}"
        )));
    }
    Ok(())
}

fn validate_type_export_names(
    scope: &str,
    types: &BTreeSet<TypeRef>,
) -> Result<(), CodegenTsError> {
    let mut names = BTreeMap::new();
    for reference in types {
        validate_export_name("type", reference.id.as_str())?;
        if let Some(previous) = names.insert(reference.id.as_str(), reference.package.as_str()) {
            return Err(CodegenTsError::ExportNameCollision(format!(
                "{scope}/{}: '{}' and '{}'",
                reference.id, previous, reference.package
            )));
        }
    }
    Ok(())
}

fn validate_export_name(kind: &str, name: &str) -> Result<(), CodegenTsError> {
    if !is_safe_js_ident(name) {
        return Err(CodegenTsError::ExportNameCollision(format!(
            "{kind} name '{name}' is not a JavaScript identifier"
        )));
    }
    Ok(())
}

fn render_types(package: &SemanticPackage, modules: &BTreeMap<String, String>) -> String {
    let own_module = &modules[package.identity().as_str()];
    let mut lines = vec!["import { codecs } from \"@oatscenter/trellis/generated\";".to_owned()];
    for dependency in package
        .types()
        .values()
        .flat_map(type_definition_refs)
        .filter(|reference| reference.package.as_str() != package.identity().as_str())
        .map(|reference| reference.package.as_str())
        .collect::<BTreeSet<_>>()
    {
        lines.push(format!(
            "import * as {} from \"./{}.ts\";",
            module_alias(&modules[dependency]),
            modules[dependency]
        ));
    }
    lines.push(String::new());
    for (id, definition) in package.types() {
        let name = id.as_str();
        lines.push(format!(
            "export type {name} = {};",
            render_type_definition(name, definition, package.identity().as_str(), modules)
        ));
        lines.push(format!(
            "export const {name}Codec: {{ decode(value: unknown): {name}; encode(value: {name}): unknown }} = codecs.recursive<{name}>(() => codecs.named({}, {}));",
            js_string(&format!("{}.{name}", package.identity())),
            render_codec_definition(definition, package.identity().as_str(), own_module, modules)
        ));
        lines.push(String::new());
    }
    if package.types().is_empty() {
        lines.push("export {};".to_owned());
    }
    format!("{}\n", lines.join("\n"))
}

fn render_type_exports(
    types: &BTreeSet<TypeRef>,
    internal_prefix: &str,
    modules: &BTreeMap<String, String>,
) -> String {
    let mut output = String::new();
    for reference in types {
        let path = format!(
            "{internal_prefix}{}.ts",
            modules[reference.package.as_str()]
        );
        writeln!(
            output,
            "export type {{ {} }} from {};",
            reference.id,
            js_string(&path)
        )
        .unwrap();
        writeln!(
            output,
            "export {{ {}Codec }} from {};",
            reference.id,
            js_string(&path)
        )
        .unwrap();
    }
    if output.is_empty() {
        output.push_str("export {};\n");
    }
    output
}

fn type_definition_refs(definition: &TypeDefinition) -> Vec<&TypeRef> {
    let mut refs = Vec::new();
    match definition {
        TypeDefinition::Model(fields) => {
            for field in fields.values() {
                expression_refs(&field.ty, &mut refs);
            }
        }
        TypeDefinition::Alias(value) => expression_refs(value, &mut refs),
        TypeDefinition::Enum(_) => {}
    }
    refs
}

fn expression_refs<'a>(value: &'a TypeExpression, refs: &mut Vec<&'a TypeRef>) {
    match value {
        TypeExpression::Named(reference) => refs.push(reference),
        TypeExpression::List(value, _)
        | TypeExpression::Map(value)
        | TypeExpression::Nullable(value)
        | TypeExpression::CursorPage(value) => expression_refs(value, refs),
        TypeExpression::Primitive(_, _) | TypeExpression::CursorQuery => {}
    }
}

fn render_type_definition(
    _name: &str,
    definition: &TypeDefinition,
    package: &str,
    modules: &BTreeMap<String, String>,
) -> String {
    match definition {
        TypeDefinition::Model(fields) => {
            let mut output = String::from("{ ");
            for (name, field) in fields {
                write!(
                    output,
                    "readonly {}{}: {}; ",
                    property_name(name),
                    if field.optional { "?" } else { "" },
                    render_type_expression(&field.ty, package, modules)
                )
                .unwrap();
            }
            output.push_str("readonly [key: string]: unknown }");
            output
        }
        TypeDefinition::Enum(values) => format!(
            "{} | (string & {{ readonly __openEnum?: never }})",
            values
                .iter()
                .map(|value| js_string(value))
                .collect::<Vec<_>>()
                .join(" | ")
        ),
        TypeDefinition::Alias(value) => render_type_expression(value, package, modules),
    }
}

fn render_type_expression(
    value: &TypeExpression,
    package: &str,
    modules: &BTreeMap<String, String>,
) -> String {
    match value {
        TypeExpression::Primitive(value, _) => match value {
            Primitive::String => "string".into(),
            Primitive::Bool => "boolean".into(),
            Primitive::Int32 | Primitive::Uint32 | Primitive::Number => "number".into(),
            Primitive::Int64 | Primitive::Uint64 => "bigint".into(),
            Primitive::Bytes => "Uint8Array".into(),
            Primitive::Timestamp => "ReturnType<typeof codecs.timestamp.decode>".into(),
            Primitive::Ulid => "ReturnType<typeof codecs.ulid.decode>".into(),
        },
        TypeExpression::Named(reference) if reference.package.as_str() == package => {
            reference.id.as_str().into()
        }
        TypeExpression::Named(reference) => format!(
            "{}.{}",
            module_alias(&modules[reference.package.as_str()]),
            reference.id
        ),
        TypeExpression::List(value, _) => {
            format!("Array<{}>", render_type_expression(value, package, modules))
        }
        TypeExpression::Map(value) => format!(
            "Record<string, {}>",
            render_type_expression(value, package, modules)
        ),
        TypeExpression::Nullable(value) => {
            format!("{} | null", render_type_expression(value, package, modules))
        }
        TypeExpression::CursorQuery => {
            "{ readonly cursor?: string; readonly limit?: number; readonly [key: string]: unknown }"
                .into()
        }
        TypeExpression::CursorPage(value) => format!(
            "{{ readonly items: Array<{}>; readonly page: {{ readonly nextCursor?: string; readonly [key: string]: unknown }}; readonly [key: string]: unknown }}",
            render_type_expression(value, package, modules)
        ),
    }
}

fn render_codec_definition(
    definition: &TypeDefinition,
    package: &str,
    own_module: &str,
    modules: &BTreeMap<String, String>,
) -> String {
    match definition {
        TypeDefinition::Model(fields) => format!(
            "codecs.model({{ {} }})",
            fields
                .iter()
                .map(|(name, field)| format!(
                    "{}: {}",
                    property_name(name),
                    if field.optional {
                        format!(
                            "codecs.optional({})",
                            render_codec_expression(&field.ty, package, own_module, modules)
                        )
                    } else {
                        render_codec_expression(&field.ty, package, own_module, modules)
                    }
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TypeDefinition::Enum(values) => format!(
            "codecs.openEnum([{}] as const)",
            values
                .iter()
                .map(|value| js_string(value))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TypeDefinition::Alias(value) => {
            render_codec_expression(value, package, own_module, modules)
        }
    }
}

fn render_codec_expression(
    value: &TypeExpression,
    package: &str,
    own_module: &str,
    modules: &BTreeMap<String, String>,
) -> String {
    match value {
        TypeExpression::Primitive(value, _) => format!(
            "codecs.{}",
            match value {
                Primitive::String => "string",
                Primitive::Bool => "bool",
                Primitive::Int32 => "i32",
                Primitive::Uint32 => "u32",
                Primitive::Int64 => "i64",
                Primitive::Uint64 => "u64",
                Primitive::Number => "f64",
                Primitive::Bytes => "bytes",
                Primitive::Timestamp => "timestamp",
                Primitive::Ulid => "ulid",
            }
        ),
        TypeExpression::Named(reference) => {
            let target = &modules[reference.package.as_str()];
            if reference.package.as_str() == package && target == own_module {
                format!("codecs.ref(() => {}Codec)", reference.id)
            } else {
                format!(
                    "codecs.ref(() => {}.{}Codec)",
                    module_alias(target),
                    reference.id
                )
            }
        }
        TypeExpression::List(value, _) => format!(
            "codecs.list({})",
            render_codec_expression(value, package, own_module, modules)
        ),
        TypeExpression::Map(value) => format!(
            "codecs.map({})",
            render_codec_expression(value, package, own_module, modules)
        ),
        TypeExpression::Nullable(value) => format!(
            "codecs.nullable({})",
            render_codec_expression(value, package, own_module, modules)
        ),
        TypeExpression::CursorQuery => "codecs.model({ cursor: codecs.optional(codecs.string), limit: codecs.optional(codecs.u32) })".into(),
        TypeExpression::CursorPage(value) => format!(
            "codecs.model({{ items: codecs.list({}), page: codecs.model({{ nextCursor: codecs.optional(codecs.string) }}) }})",
            render_codec_expression(value, package, own_module, modules)
        ),
    }
}

fn render_api(
    graph: &PackageGraph,
    api: &trellis_idl::ApiDefinition,
    modules: &BTreeMap<String, String>,
) -> Result<String, CodegenTsError> {
    let package = api
        .identity()
        .as_str()
        .rsplit_once('.')
        .map(|(package, _)| package)
        .ok_or_else(|| CodegenTsError::MissingReference(api.identity().to_string()))?;
    let mut lines = vec![if api.errors().is_empty() {
        "import { apiDescriptor } from \"@oatscenter/trellis/generated\";".to_owned()
    } else {
        "import { apiDescriptor, TrellisError } from \"@oatscenter/trellis/generated\";".to_owned()
    }];
    if !api.errors().is_empty() {
        lines.push(
            "import type { SerializableErrorData } from \"@oatscenter/trellis/generated\";"
                .to_owned(),
        );
    }
    for module in modules.values() {
        lines.push(format!(
            "import * as {} from \"../../types/_internal/{module}.ts\";",
            module_alias(module)
        ));
    }
    lines.push(String::new());
    for (name, payload_type) in api.errors() {
        let data = format!("{name}Data");
        let qualified = format!("{}::{name}", api.identity());
        let payload = payload_type
            .as_ref()
            .map(|value| format!(" & {}", type_ref(value, package, modules)))
            .unwrap_or_default();
        lines.push(format!(
            "export type {data} = SerializableErrorData & {{ readonly type: {} }}{payload};",
            js_string(&qualified)
        ));
        lines.push(format!(
            "export class {name} extends TrellisError<{data}> {{"
        ));
        lines.push(format!(
            "  static readonly type = {} as const;",
            js_string(&qualified)
        ));
        if let Some(payload) = payload_type {
            let codec = type_codec(payload, package, modules);
            lines.push(format!(
                "  static readonly payloadCodec: typeof {codec} = {codec};"
            ));
        }
        lines.push(format!(
            "  override readonly name = {} as const;",
            js_string(name)
        ));
        lines.push(format!("  readonly data: {data};"));
        lines.push(format!("  constructor(data: {data}) {{"));
        lines.push("    super(data.message, { id: data.id, ...(data.context !== undefined ? { context: data.context } : {}) });".into());
        lines.push("    this.data = data;".into());
        lines.push("  }".into());
        lines.push(if payload_type.is_some() {
            format!(
                "  static fromSerializable(data: unknown): {name} {{ const error = data as SerializableErrorData; return new {name}({{ ...error, ...{name}.payloadCodec.decode(data) }} as {data}); }}"
            )
        } else {
            format!(
                "  static fromSerializable(data: unknown): {name} {{ return new {name}(data as {data}); }}"
            )
        });
        lines.push(format!(
            "  override toSerializable(): {data} {{ return this.data; }}"
        ));
        lines.push("}".into());
        lines.push(String::new());
    }
    for (id, action) in api.actions() {
        let base = action_type_base(id);
        match action {
            ActionDefinition::Rpc { input, output, .. } => {
                lines.push(format!(
                    "export type {base}Input = {};",
                    type_ref(input, package, modules)
                ));
                lines.push(format!(
                    "export type {base}Output = {};",
                    type_ref(output, package, modules)
                ));
            }
            ActionDefinition::Operation {
                input,
                output,
                progress,
                update,
                signals,
                ..
            } => {
                lines.push(format!(
                    "export type {base}Input = {};",
                    type_ref(input, package, modules)
                ));
                lines.push(format!(
                    "export type {base}Output = {};",
                    type_ref(output, package, modules)
                ));
                if let Some(progress) = progress {
                    lines.push(format!(
                        "export type {base}Progress = {};",
                        type_ref(progress, package, modules)
                    ));
                }
                // The live update channel reuses the progress schema when no
                // dedicated update schema is declared, preserving existing contracts.
                if let Some(update) = update {
                    lines.push(format!(
                        "export type {base}Update = {};",
                        type_ref(update, package, modules)
                    ));
                } else if let Some(progress) = progress {
                    lines.push(format!(
                        "export type {base}Update = {};",
                        type_ref(progress, package, modules)
                    ));
                }
                for (name, ty) in signals {
                    lines.push(format!(
                        "export type {base}{}Signal = {};",
                        pascal(name),
                        type_ref(ty, package, modules)
                    ));
                }
            }
            ActionDefinition::Event { payload, .. } => {
                lines.push(format!(
                    "export type {base}Event = {};",
                    type_ref(payload, package, modules)
                ));
            }
            ActionDefinition::Live { input, event } => {
                lines.push(format!(
                    "export type {base}Input = {};",
                    type_ref(input, package, modules)
                ));
                lines.push(format!(
                    "export type {base}Event = {};",
                    type_ref(event, package, modules)
                ));
            }
        }
    }
    lines.push(String::new());
    lines.push("const __api: {".into());
    lines.push(format!(
        "  readonly identity: {};",
        js_string(api.identity().as_str())
    ));
    lines.push("  readonly actions: {".into());
    for (id, action) in api.actions() {
        lines.push(format!(
            "    readonly {}: {};",
            js_string(&descriptor_name(id)),
            render_action_type(action, id, package, modules)
        ));
    }
    lines.push("  };".into());
    lines.push("  readonly packageEvidence: unknown;".into());
    lines.push("} = apiDescriptor({".into());
    lines.push(format!(
        "  identity: {},",
        js_string(api.identity().as_str())
    ));
    lines.push("  actions: {".into());
    for (id, action) in api.actions() {
        lines.push(format!(
            "    {}: {},",
            js_string(&descriptor_name(id)),
            render_action(action, id, package, modules)
        ));
    }
    lines.push("  },".into());
    lines.push(format!("  packageEvidence: {},", package_evidence(graph)?));
    lines.push("});".into());
    lines.push("export const API: typeof __api = __api;".into());
    lines.push(format!(
        "export const API_DIGEST = {} as const;",
        js_string(&trellis_idl::api_digest(graph, api.identity()).map_err(idl_error)?)
    ));
    lines.push(String::new());
    Ok(lines.join("\n"))
}

fn render_action(
    action: &ActionDefinition,
    id: &ActionId,
    package: &str,
    modules: &BTreeMap<String, String>,
) -> String {
    let descriptor = js_string(&descriptor_name(id));
    match action {
        ActionDefinition::Rpc { input, output, errors, download, pagination } => format!(
            "{{ kind: \"rpc\", descriptorName: {descriptor}, input: {}, output: {}, errors: [{}], download: {download}, pagination: {} }}",
            type_codec(input, package, modules),
            type_codec(output, package, modules),
            errors.iter().map(|error| error.to_owned()).collect::<Vec<_>>().join(", "),
            if pagination.is_some() { "\"cursor\"" } else { "undefined" }
        ),
        ActionDefinition::Operation { input, output, progress, update, errors, signals, upload } => format!(
            "{{ kind: \"operation\", descriptorName: {descriptor}, input: {}, output: {}, progress: {}, update: {}, errors: [{}], signals: {{ {} }}, upload: {upload} }}",
            type_codec(input, package, modules),
            type_codec(output, package, modules),
            progress.as_ref().map(|value| type_codec(value, package, modules)).unwrap_or_else(|| "undefined".into()),
            update.as_ref().or(progress.as_ref()).map(|value| type_codec(value, package, modules)).unwrap_or_else(|| "undefined".into()),
            errors.iter().map(|error| error.to_owned()).collect::<Vec<_>>().join(", "),
            signals.iter().map(|(name, ty)| format!("{}: {}", property_name(name), type_codec(ty, package, modules))).collect::<Vec<_>>().join(", ")
        ),
        ActionDefinition::Event { payload, parameters } => format!(
            "{{ kind: \"event\", descriptorName: {descriptor}, payload: {}, parameters: {} }}",
            type_codec(payload, package, modules),
            serde_json::to_string(parameters).expect("event parameters")
        ),
        ActionDefinition::Live { input, event } => format!(
            "{{ kind: \"live\", descriptorName: {descriptor}, input: {}, event: {} }}",
            type_codec(input, package, modules),
            type_codec(event, package, modules)
        ),
    }
}

fn render_action_type(
    action: &ActionDefinition,
    id: &ActionId,
    package: &str,
    modules: &BTreeMap<String, String>,
) -> String {
    let common = format!(
        "readonly kind: {}; readonly descriptorName: {}",
        js_string(match id.kind {
            ActionKind::Rpc => "rpc",
            ActionKind::Operation => "operation",
            ActionKind::Event => "event",
            ActionKind::Live => "live",
        }),
        js_string(&descriptor_name(id))
    );
    match action {
        ActionDefinition::Rpc { input, output, errors, download, pagination } => format!(
            "{{ {common}; readonly input: typeof {}; readonly output: typeof {}; readonly errors: readonly [{}]; readonly download: {download}; readonly pagination: {} }}",
            type_codec(input, package, modules),
            type_codec(output, package, modules),
            errors.iter().map(|error| format!("typeof {error}")).collect::<Vec<_>>().join(", "),
            if pagination.is_some() { "\"cursor\"" } else { "undefined" }
        ),
        ActionDefinition::Operation { input, output, progress, update, errors, signals, upload } => format!(
            "{{ {common}; readonly input: typeof {}; readonly output: typeof {}; readonly progress: {}; readonly update: {}; readonly errors: readonly [{}]; readonly signals: {{ {} }}; readonly upload: {upload} }}",
            type_codec(input, package, modules),
            type_codec(output, package, modules),
            progress.as_ref().map(|value| format!("typeof {}", type_codec(value, package, modules))).unwrap_or_else(|| "undefined".into()),
            update.as_ref().or(progress.as_ref()).map(|value| format!("typeof {}", type_codec(value, package, modules))).unwrap_or_else(|| "undefined".into()),
            errors.iter().map(|error| format!("typeof {error}")).collect::<Vec<_>>().join(", "),
            signals.iter().map(|(name, ty)| format!("readonly {}: typeof {}", property_name(name), type_codec(ty, package, modules))).collect::<Vec<_>>().join("; ")
        ),
        ActionDefinition::Event { payload, parameters } => format!(
            "{{ {common}; readonly payload: typeof {}; readonly parameters: readonly {} }}",
            type_codec(payload, package, modules),
            render_string_matrix_type(parameters)
        ),
        ActionDefinition::Live { input, event } => format!(
            "{{ {common}; readonly input: typeof {}; readonly event: typeof {} }}",
            type_codec(input, package, modules),
            type_codec(event, package, modules)
        ),
    }
}

fn render_string_matrix_type(values: &[Vec<String>]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|path| format!(
                "readonly [{}]",
                path.iter()
                    .map(|part| js_string(part))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn app_module_name(package: &SemanticPackage, request: &AppRequestDefinition) -> String {
    request
        .identity()
        .as_str()
        .strip_prefix(&format!("{}.", package.identity()))
        .expect("request belongs to package")
        .replace('.', "_")
}

fn render_app_request(
    graph: &PackageGraph,
    request: &AppRequestDefinition,
    modules: &BTreeMap<String, String>,
) -> Result<String, CodegenTsError> {
    let mut lines = vec![
        "import type { AppRequestDescriptor } from \"@oatscenter/trellis/generated\";".to_owned(),
    ];
    let api_modules = api_modules(graph);
    let mut aliases = BTreeMap::new();
    for (index, id) in request.uses().keys().enumerate() {
        let alias = format!("Api{index}");
        lines.push(format!(
            "import * as {alias} from \"../../apis/{}/mod.ts\";",
            api_modules[id.as_str()].1
        ));
        aliases.insert(id, alias);
    }
    for module in modules.values() {
        lines.push(format!(
            "import * as {} from \"../../types/_internal/{module}.ts\";",
            module_alias(module)
        ));
    }
    let kind = match request.kind() {
        AppKind::Browser => "browser",
        AppKind::Native => "native",
    };
    let required = request
        .uses()
        .values()
        .flat_map(|selection| &selection.required_capabilities)
        .collect::<BTreeSet<_>>();
    let optional = request
        .uses()
        .values()
        .flat_map(|selection| &selection.optional_capabilities)
        .collect::<BTreeSet<_>>();
    let required = required
        .iter()
        .map(|id| js_string(id.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    let optional = optional
        .iter()
        .map(|id| js_string(id.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    let mut use_types = Vec::new();
    let mut use_values = Vec::new();
    for (id, selection) in request.uses() {
        let api = find_api(graph, id)?;
        let mut action_types = Vec::new();
        let mut action_values = Vec::new();
        for selected in &selection.actions {
            let required = selection
                .required_capabilities
                .iter()
                .any(|capability| api.capabilities()[capability].allows.contains(selected));
            let alternatives = if required {
                String::new()
            } else {
                selection
                    .optional_capabilities
                    .iter()
                    .filter(|capability| api.capabilities()[*capability].allows.contains(selected))
                    .map(|id| js_string(id.as_str()))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let descriptor = js_string(&descriptor_name(&selected.action));
            let direction = js_string(direction(selected.direction));
            action_types.push(format!("{{ readonly descriptorName: {descriptor}; readonly direction: {direction}; readonly optionalCapabilities: readonly [{alternatives}] }}"));
            action_values.push(format!("{{ descriptorName: {descriptor}, direction: {direction}, optionalCapabilities: [{alternatives}] }}"));
        }
        let optional = selection
            .optional_capabilities
            .iter()
            .map(|id| js_string(id.as_str()))
            .collect::<Vec<_>>()
            .join(", ");
        use_types.push(format!("{{ readonly api: typeof {}.API; readonly actions: readonly [{}]; readonly optionalCapabilities: readonly [{optional}] }}", aliases[id], action_types.join(", ")));
        use_values.push(format!(
            "{{ api: {}.API, actions: [{}], optionalCapabilities: [{optional}] }}",
            aliases[id],
            action_values.join(", ")
        ));
    }
    lines.push("/** Developer selections and local State codecs; never principal installation evidence. */".into());
    lines.push(format!("export const request: AppRequestDescriptor & {{ readonly id: {}; readonly kind: {}; readonly requiredCapabilities: readonly [{required}]; readonly optionalCapabilities: readonly [{optional}]; readonly uses: readonly [{}]; readonly states: {{", js_string(request.identity().as_str()), js_string(kind), use_types.join(", ")));
    for (name, state) in request.states() {
        lines.push(format!(
            "readonly {}: {};",
            property_name(name.as_str()),
            render_resource_type(state, modules)
        ));
    }
    lines.push(format!("}} }} = {{ id: {}, kind: {}, requiredCapabilities: [{required}], optionalCapabilities: [{optional}], uses: [{}], states: {{", js_string(request.identity().as_str()), js_string(kind), use_values.join(", ")));
    for (name, state) in request.states() {
        lines.push(format!(
            "{}: {},",
            property_name(name.as_str()),
            render_resource(state, modules)
        ));
    }
    lines.push("} };\nexport type Request = typeof request;\nexport default request;\n".into());
    Ok(lines.join("\n"))
}

fn render_participant_tree(
    graph: &PackageGraph,
    package: &SemanticPackage,
    participant: &ParticipantDefinition,
    directory: PathBuf,
    modules: &BTreeMap<String, String>,
    participant_types: &BTreeMap<&trellis_idl::ParticipantId, BTreeSet<TypeRef>>,
    sources: &mut Vec<GeneratedTsSource>,
) -> Result<(), CodegenTsError> {
    let private_types = participant_types
        .get(participant.identity())
        .expect("participant type closure");
    sources.push(GeneratedTsSource {
        path: directory.join("types.ts"),
        contents: render_type_exports(private_types, "../../types/_internal/", modules),
    });
    sources.push(GeneratedTsSource {
        path: directory.join("mod.ts"),
        contents: render_participant(graph, package, participant, modules)?,
    });
    Ok(())
}

fn render_participant(
    graph: &PackageGraph,
    package: &SemanticPackage,
    participant: &ParticipantDefinition,
    modules: &BTreeMap<String, String>,
) -> Result<String, CodegenTsError> {
    let path = participant
        .identity()
        .as_str()
        .strip_prefix(&format!("{}.", graph.root().as_str()))
        .ok_or_else(|| CodegenTsError::MissingReference(participant.identity().to_string()))?;
    let api_prefix = "../../";
    let types_prefix = "../../";
    let mut lines =
        vec!["import { participantDescriptor } from \"@oatscenter/trellis/generated\";".to_owned()];
    lines.push("import type { ParticipantJobsFromResources, ParticipantKvFromResources, RuntimeApiFromGenerated } from \"@oatscenter/trellis/generated\";".to_owned());
    lines.push("import * as types from \"./types.ts\";".to_owned());
    lines.push("export { types };".to_owned());
    let referenced = participant
        .implements()
        .iter()
        .chain(participant.uses().keys())
        .collect::<BTreeSet<_>>();
    let mut aliases = BTreeMap::new();
    let api_modules = api_modules(graph);
    for (index, id) in referenced.iter().enumerate() {
        let api = find_api(graph, id)?;
        let alias = format!("Api{index}");
        lines.push(format!(
            "import * as {alias} from \"{api_prefix}apis/{}/mod.ts\";",
            api_modules[api.identity().as_str()].1
        ));
        aliases.insert(id.as_str(), alias);
    }
    let mut action_name_counts = BTreeMap::new();
    for id in &referenced {
        for action in find_api(graph, id)?.actions().keys() {
            *action_name_counts
                .entry(action.name.to_ascii_lowercase())
                .or_insert(0usize) += 1;
        }
    }
    let mut action_names = Vec::new();
    for id in &referenced {
        let api = find_api(graph, id)?;
        for action in api.actions().keys() {
            let descriptor = descriptor_name(action);
            let name = if action_name_counts[&action.name.to_ascii_lowercase()] > 1 {
                format!("{}.{}", api.name(), action.name)
            } else {
                action.name.clone()
            };
            action_names.push((format!("{}:{descriptor}", id.as_str()), name));
        }
    }
    for package_id in participant_resource_packages(participant) {
        let module = &modules[package_id];
        lines.push(format!(
            "import * as {} from \"{types_prefix}types/_internal/{module}.ts\";",
            module_alias(module)
        ));
    }
    if let Some(companion) = participant.companion() {
        let child = package
            .app_requests()
            .get(&companion.request)
            .ok_or_else(|| CodegenTsError::MissingReference(companion.request.to_string()))?;
        let child_module = app_module_name(package, child);
        lines.push(format!(
            "import * as Companion from \"{api_prefix}apps/{child_module}/mod.ts\";"
        ));
        lines.push(format!(
            "export * as {} from \"{api_prefix}apps/{child_module}/mod.ts\";",
            child.name()
        ));
    }
    lines.push(String::new());
    lines.push("type __ActionNames = {".into());
    for (key, name) in &action_names {
        lines.push(format!(
            "  readonly {}: {};",
            js_string(key),
            js_string(name)
        ));
    }
    lines.push("};".into());
    lines.push("type __Resources = {".into());
    for (name, resource) in participant.resources() {
        lines.push(format!(
            "  readonly {}: {};",
            property_name(name.as_str()),
            render_resource_type(resource, modules)
        ));
    }
    lines.push("};".into());
    lines.push("const __participant: {".into());
    lines.push("  readonly digest: string;".into());
    lines.push("  readonly requiredCapabilities: readonly string[];".into());
    lines.push("  readonly optionalCapabilities: readonly string[];".into());
    lines.push(format!(
        "  readonly kind: {}; readonly id: {}; readonly identity: {}; readonly path: {};",
        js_string(participant_kind(participant.kind())),
        js_string(participant.identity().as_str()),
        js_string(participant.identity().as_str()),
        js_string(path)
    ));
    lines.push(format!(
        "  readonly implements: readonly [{}];",
        participant
            .implements()
            .iter()
            .map(|id| format!("typeof {}.API", aliases[id.as_str()]))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    let needs = graph.participant_needs(participant.identity());
    lines.push("  readonly uses: readonly [".into());
    for (api, selection) in participant.uses() {
        lines.push(format!(
            "    {{ readonly api: typeof {}.API; readonly actions: readonly [",
            aliases[api.as_str()]
        ));
        for action in &selection.actions {
            let alternatives = needs
                .and_then(|needs| {
                    needs
                        .optional_action_capabilities()
                        .get(&(api.clone(), action.clone()))
                })
                .map(|capabilities| {
                    capabilities
                        .iter()
                        .map(|value| js_string(value.as_str()))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            lines.push(format!(
                "      {{ readonly descriptorName: {}; readonly direction: {}; readonly optionalCapabilities: readonly [{}]; }},",
                js_string(&descriptor_name(&action.action)),
                js_string(direction(action.direction)),
                alternatives
            ));
        }
        lines.push(format!(
            "    ]; readonly optionalCapabilities: readonly [{}]; }},",
            selection
                .optional_capabilities
                .iter()
                .map(|value| js_string(value.as_str()))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    lines.push("  ];".into());
    lines.push("  readonly actionNames: __ActionNames;".into());
    lines.push("  readonly resources: __Resources;".into());
    lines.push("  readonly __runtimeTypes?: {".into());
    lines.push(format!(
        "    readonly ownedApi: RuntimeApiFromGenerated<{}, __ActionNames>;",
        type_union(
            participant
                .implements()
                .iter()
                .map(|id| format!("typeof {}.API", aliases[id.as_str()]))
                .collect::<Vec<_>>()
        )
    ));
    lines.push(format!(
        "    readonly api: RuntimeApiFromGenerated<{}, __ActionNames>;",
        type_union(
            referenced
                .iter()
                .map(|id| format!("typeof {}.API", aliases[id.as_str()]))
                .collect::<Vec<_>>()
        )
    ));
    lines.push("    readonly jobs: ParticipantJobsFromResources<__Resources>;".into());
    lines.push("    readonly kv: ParticipantKvFromResources<__Resources>;".into());
    lines.push("  };".into());
    if let Some(companion) = participant.companion() {
        lines.push(format!(
            "  readonly companion: {{ readonly request: typeof Companion.request; readonly availability: {}; }};",
            availability(companion.optional)
        ));
    }
    lines.push("  readonly packageEvidence: unknown;".into());
    lines.push("} = participantDescriptor({".into());
    lines.push(format!(
        "  kind: {},",
        js_string(participant_kind(participant.kind()))
    ));
    lines.push(format!(
        "  identity: {},",
        js_string(participant.identity().as_str())
    ));
    lines.push(format!(
        "  id: {},",
        js_string(participant.identity().as_str())
    ));
    lines.push(format!("  path: {},", js_string(path)));
    lines.push(format!(
        "  digest: {},",
        js_string(
            &trellis_idl::participant_digest(graph, participant.identity()).map_err(idl_error)?
        )
    ));
    lines.push(format!(
        "  implements: [{}],",
        participant
            .implements()
            .iter()
            .map(|id| format!("{}.API", aliases[id.as_str()]))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    let needs = graph.participant_needs(participant.identity());
    lines.push("  uses: [".into());
    for (api, selection) in participant.uses() {
        lines.push(format!(
            "    {{ api: {}.API, actions: [",
            aliases[api.as_str()]
        ));
        for action in &selection.actions {
            // Optional capability alternatives are per action, derived from the
            // same needs semantics the server compiles: an action covered by any
            // non-optional path is never gated, and otherwise only the optional
            // capabilities that actually cover it are alternatives.
            let alternatives = needs
                .and_then(|needs| {
                    needs
                        .optional_action_capabilities()
                        .get(&(api.clone(), action.clone()))
                })
                .map(|capabilities| {
                    capabilities
                        .iter()
                        .map(|value| js_string(value.as_str()))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            lines.push(format!(
                "      {{ descriptorName: {}, direction: {}, optionalCapabilities: [{}] }},",
                js_string(&descriptor_name(&action.action)),
                js_string(direction(action.direction)),
                alternatives
            ));
        }
        lines.push(format!(
            "    ], optionalCapabilities: [{}] }},",
            selection
                .optional_capabilities
                .iter()
                .map(|value| js_string(value.as_str()))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    lines.push("  ],".into());
    let mut required_capabilities = Vec::new();
    let mut optional_capabilities = Vec::new();
    if let Some(needs) = needs {
        for capability in needs.required_capabilities() {
            required_capabilities.push(js_string(capability.as_str()));
        }
        optional_capabilities.extend(
            needs
                .optional_capabilities()
                .iter()
                .map(|capability| js_string(capability.as_str())),
        );
    }
    let required_capabilities = required_capabilities.join(", ");
    lines.push(format!(
        "  requiredCapabilities: [{required_capabilities}],"
    ));
    lines.push(format!(
        "  optionalCapabilities: [{}],",
        optional_capabilities.join(", ")
    ));
    lines.push("  actionNames: {".into());
    for (key, name) in &action_names {
        lines.push(format!("    {}: {},", js_string(key), js_string(name)));
    }
    lines.push("  },".into());
    lines.push("  resources: {".into());
    for (name, resource) in participant.resources() {
        lines.push(format!(
            "    {}: {},",
            property_name(name.as_str()),
            render_resource(resource, modules)
        ));
    }
    lines.push("  },".into());
    if let Some(companion) = participant.companion() {
        lines.push(format!(
            "  companion: {{ request: Companion.request, availability: {} }},",
            availability(companion.optional)
        ));
    }
    lines.push(format!("  packageEvidence: {},", package_evidence(graph)?));
    lines.push("});".into());
    lines.push("export const participant: {".into());
    lines.push("  readonly kind: typeof __participant.kind;".into());
    lines.push("  readonly id: typeof __participant.id;".into());
    lines.push("  readonly identity: typeof __participant.identity;".into());
    lines.push("  readonly path: typeof __participant.path;".into());
    lines.push("  readonly digest: typeof __participant.digest;".into());
    lines
        .push("  readonly requiredCapabilities: typeof __participant.requiredCapabilities;".into());
    lines.push("  readonly implements: typeof __participant.implements;".into());
    lines.push("  readonly uses: typeof __participant.uses;".into());
    lines
        .push("  readonly optionalCapabilities: typeof __participant.optionalCapabilities;".into());
    lines.push("  readonly actionNames: typeof __participant.actionNames;".into());
    lines.push("  readonly resources: typeof __participant.resources;".into());
    lines.push("  readonly __runtimeTypes?: typeof __participant.__runtimeTypes;".into());
    if participant.companion().is_some() {
        lines.push("  readonly companion: typeof __participant.companion;".into());
    }
    lines.push("  readonly packageEvidence: unknown;".into());
    lines.push("} = __participant;".into());
    lines.push(format!(
        "export const PARTICIPANT_DIGEST = {} as const;",
        js_string(
            &trellis_idl::participant_digest(graph, participant.identity()).map_err(idl_error)?
        )
    ));
    render_participant_types(participant, modules, &mut lines);
    lines.push("export default participant;".into());
    lines.push(String::new());
    Ok(lines.join("\n"))
}

fn render_participant_types(
    participant: &ParticipantDefinition,
    modules: &BTreeMap<String, String>,
    lines: &mut Vec<String>,
) {
    lines.push("export type Participant = typeof participant;".into());
    lines.push("export type ResourceDescriptors = typeof participant.resources;".into());
    for (name, resource) in participant.resources() {
        let exported = pascal(name.as_str());
        let optional = match resource {
            ResourceDefinition::State { optional, .. }
            | ResourceDefinition::Kv { optional, .. }
            | ResourceDefinition::Store { optional, .. }
            | ResourceDefinition::Job { optional, .. }
            | ResourceDefinition::Consumer { optional, .. } => *optional,
        };
        lines.push(format!(
            "export type {exported}Resource = ResourceDescriptors[{}];",
            js_string(name.as_str())
        ));
        lines.push(format!(
            "export type {exported}Handle<Handle> = Handle{};",
            if optional { " | undefined" } else { "" }
        ));
    }
    lines.push("export type ResourceHandles<Handles extends { readonly [Name in keyof ResourceDescriptors]: unknown }> =".into());
    lines.push("  & { readonly [Name in keyof ResourceDescriptors as ResourceDescriptors[Name][\"availability\"] extends \"required\" ? Name : never]: Handles[Name] }".into());
    lines.push("  & { readonly [Name in keyof ResourceDescriptors as ResourceDescriptors[Name][\"availability\"] extends \"optional\" ? Name : never]: Handles[Name] | undefined };".into());

    lines.push("export type Availability = Readonly<{".into());
    lines.push("  resources: Readonly<{".into());
    for (name, resource) in participant.resources() {
        if matches!(
            resource,
            ResourceDefinition::State { optional: true, .. }
                | ResourceDefinition::Kv { optional: true, .. }
                | ResourceDefinition::Store { optional: true, .. }
                | ResourceDefinition::Job { optional: true, .. }
                | ResourceDefinition::Consumer { optional: true, .. }
        ) {
            lines.push(format!(
                "    readonly {}: boolean;",
                property_name(name.as_str())
            ));
        }
    }
    lines.push("  }>;".into());
    lines.push("  capabilities: Readonly<{".into());
    for selection in participant.uses().values() {
        for capability in &selection.optional_capabilities {
            lines.push(format!(
                "    readonly {}: boolean;",
                js_string(capability.as_str())
            ));
        }
    }
    lines.push("  }>;".into());
    lines.push("}>;".into());
    lines.push("export type ParticipantFacade<Handles extends { readonly [Name in keyof ResourceDescriptors]: unknown }> = Readonly<{".into());
    lines.push("  participant: Participant;".into());
    lines.push("  resources: ResourceHandles<Handles>;".into());
    lines.push("  availability(): Availability;".into());
    lines.push("  watchAvailability(): AsyncIterable<Availability>;".into());
    lines.push("}>;".into());

    let migrated = participant
        .resources()
        .iter()
        .filter_map(|(name, resource)| match resource {
            ResourceDefinition::State {
                schema, accepts, ..
            }
            | ResourceDefinition::Kv {
                schema, accepts, ..
            } if !accepts.is_empty() => Some((name, schema, accepts)),
            _ => None,
        })
        .collect::<Vec<_>>();
    if !migrated.is_empty() {
        lines.push("export type MigrationOptions = Readonly<{".into());
        for (name, current, accepts) in migrated {
            lines.push(format!(
                "  readonly {}: Readonly<{{",
                property_name(name.as_str())
            ));
            for historic in accepts {
                lines.push(format!(
                    "    readonly {}: (value: {}) => {};",
                    historic.version,
                    type_ref(&historic.ty, "", modules),
                    type_ref(current, "", modules)
                ));
            }
            lines.push("  }>;".into());
        }
        lines.push("}>;".into());
        lines.push(
            "export type ConnectionOptions = Readonly<{ migrations: MigrationOptions }>;".into(),
        );
    }
}

fn participant_resource_packages(participant: &ParticipantDefinition) -> BTreeSet<&str> {
    let mut packages = BTreeSet::new();
    for resource in participant.resources().values() {
        match resource {
            ResourceDefinition::State {
                schema, accepts, ..
            }
            | ResourceDefinition::Kv {
                schema, accepts, ..
            } => {
                packages.insert(schema.package.as_str());
                packages.extend(accepts.iter().map(|value| value.ty.package.as_str()));
            }
            ResourceDefinition::Job {
                payload,
                result,
                update,
                ..
            } => {
                packages.insert(payload.package.as_str());
                packages.extend(result.iter().map(|value| value.package.as_str()));
                packages.extend(update.iter().map(|value| value.package.as_str()));
            }
            ResourceDefinition::Store { .. } | ResourceDefinition::Consumer { .. } => {}
        }
    }
    packages
}

fn render_resource(resource: &ResourceDefinition, modules: &BTreeMap<String, String>) -> String {
    match resource {
        ResourceDefinition::State { optional, schema, version, accepts, .. } => format!(
            "{{ kind: \"state\", availability: {}, codec: {}, version: {version}, migrations: {{ {} }} }}",
            availability(*optional),
            type_codec(schema, "", modules),
            migrations(accepts, modules)
        ),
        ResourceDefinition::Kv { optional, schema, version, accepts, history, ttl_ms, desired_max_value, .. } => format!(
            "{{ kind: \"kv\", availability: {}, codec: {}, version: {version}, migrations: {{ {} }}, history: {history}, ttlMs: {ttl_ms}, desiredMaxValue: {} }}",
            availability(*optional),
            type_codec(schema, "", modules),
            migrations(accepts, modules),
            option_number(*desired_max_value)
        ),
        ResourceDefinition::Store { optional, ttl_ms, desired_max_object, desired_max_total, .. } => format!(
            "{{ kind: \"store\", availability: {}, ttlMs: {ttl_ms}, desiredMaxObject: {}, desiredMaxTotal: {} }}",
            availability(*optional), option_number(*desired_max_object), option_number(*desired_max_total)
        ),
        ResourceDefinition::Job { optional, payload, result, update, deadline_ms, retry, .. } => format!(
            "{{ kind: \"job\", availability: {}, payload: {}, result: {}, update: {}, deadlineMs: {}, retry: {} }}",
            availability(*optional),
            type_codec(payload, "", modules),
            result.as_ref().map(|value| type_codec(value, "", modules)).unwrap_or_else(|| "undefined".into()),
            update.as_ref().map(|value| type_codec(value, "", modules)).unwrap_or_else(|| "undefined".into()),
            option_number(*deadline_ms),
            retry.as_ref().map(|value| serde_json::json!({"attempts":value.attempts,"backoffMs":value.backoff_ms}).to_string()).unwrap_or_else(|| "undefined".into())
        ),
        ResourceDefinition::Consumer { optional, events, concurrency, replay, retry, .. } => format!(
            "{{ kind: \"consumer\", availability: {}, events: {}, concurrency: {concurrency}, replay: {}, retry: {} }}",
            availability(*optional),
            serde_json::to_string(&events.iter().map(|(api, event)| [api.as_str(), event]).collect::<Vec<_>>()).expect("consumer events"),
            js_string(&format!("{:?}", replay).to_lowercase()),
            retry.as_ref().map(|value| serde_json::json!({"attempts":value.attempts,"backoffMs":value.backoff_ms}).to_string()).unwrap_or_else(|| "undefined".into())
        ),
    }
}

fn render_resource_type(
    resource: &ResourceDefinition,
    modules: &BTreeMap<String, String>,
) -> String {
    let (kind, optional) = match resource {
        ResourceDefinition::State { optional, .. } => ("state", optional),
        ResourceDefinition::Kv { optional, .. } => ("kv", optional),
        ResourceDefinition::Store { optional, .. } => ("store", optional),
        ResourceDefinition::Job { optional, .. } => ("job", optional),
        ResourceDefinition::Consumer { optional, .. } => ("consumer", optional),
    };
    let mut fields = vec![
        format!("readonly kind: {}", js_string(kind)),
        format!("readonly availability: {}", availability(*optional)),
    ];
    match resource {
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
        } => {
            fields.push(format!(
                "readonly codec: typeof {}",
                type_codec(schema, "", modules)
            ));
            fields.push(format!("readonly version: {version}"));
            fields.push(format!(
                "readonly migrations: {{ {} }}",
                accepts
                    .iter()
                    .map(|value| format!(
                        "readonly {}: typeof {}",
                        value.version,
                        type_codec(&value.ty, "", modules)
                    ))
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        ResourceDefinition::Job {
            payload,
            result,
            update,
            ..
        } => {
            fields.push(format!(
                "readonly payload: typeof {}",
                type_codec(payload, "", modules)
            ));
            fields.push(format!(
                "readonly result: {}",
                result
                    .as_ref()
                    .map(|value| format!("typeof {}", type_codec(value, "", modules)))
                    .unwrap_or_else(|| "undefined".into())
            ));
            fields.push(format!(
                "readonly update: {}",
                update
                    .as_ref()
                    .map(|value| format!("typeof {}", type_codec(value, "", modules)))
                    .unwrap_or_else(|| "undefined".into())
            ));
        }
        ResourceDefinition::Store { .. } | ResourceDefinition::Consumer { .. } => {}
    }
    format!("{{ {} }}", fields.join("; "))
}

fn migrations(
    accepts: &[trellis_idl::HistoricRepresentation],
    modules: &BTreeMap<String, String>,
) -> String {
    accepts
        .iter()
        .map(|value| format!("{}: {}", value.version, type_codec(&value.ty, "", modules)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn package_evidence(graph: &PackageGraph) -> Result<String, CodegenTsError> {
    let packages = graph
        .packages()
        .iter()
        .map(|(id, package)| {
            Ok(serde_json::json!({
                "name": id.as_str(),
                "version": package.version().to_string(),
                "digest": graph.digest(id).expect("graph package digest"),
                "source": trellis_idl::canonical_package(graph, id, CanonicalMode::Presentation)
                    .map_err(idl_error)?,
            }))
        })
        .collect::<Result<Vec<_>, CodegenTsError>>()?;
    Ok(serde_json::json!({
        "rootPackage": graph.root().as_str(),
        "rootDigest": graph.root_digest(),
        "packages": packages,
    })
    .to_string())
}

fn api_reachable_types(graph: &PackageGraph) -> BTreeSet<TypeRef> {
    let mut result = BTreeSet::new();
    for package in graph.packages().values() {
        for api in package.apis().values() {
            for reference in api.errors().values().flatten() {
                collect_type(graph, reference, &mut result);
            }
            for action in api.actions().values() {
                for reference in action_references(action) {
                    collect_type(graph, reference, &mut result);
                }
            }
        }
    }
    result
}

fn participant_reachable_types(
    graph: &PackageGraph,
    participant: &ParticipantDefinition,
) -> BTreeSet<TypeRef> {
    let mut result = BTreeSet::new();
    for resource in participant.resources().values() {
        match resource {
            ResourceDefinition::State {
                schema, accepts, ..
            }
            | ResourceDefinition::Kv {
                schema, accepts, ..
            } => {
                collect_type(graph, schema, &mut result);
                for historic in accepts {
                    collect_type(graph, &historic.ty, &mut result);
                }
            }
            ResourceDefinition::Job {
                payload,
                result: output,
                update,
                ..
            } => {
                collect_type(graph, payload, &mut result);
                for reference in output.iter().chain(update) {
                    collect_type(graph, reference, &mut result);
                }
            }
            ResourceDefinition::Store { .. } | ResourceDefinition::Consumer { .. } => {}
        }
    }
    result
}

fn collect_type(graph: &PackageGraph, reference: &TypeRef, result: &mut BTreeSet<TypeRef>) {
    if !result.insert(reference.clone()) {
        return;
    }
    let definition = &graph
        .package(&reference.package)
        .expect("resolved type package")
        .types()[&reference.id];
    match definition {
        TypeDefinition::Model(fields) => {
            for field in fields.values() {
                collect_expression(graph, &field.ty, result);
            }
        }
        TypeDefinition::Alias(expression) => collect_expression(graph, expression, result),
        TypeDefinition::Enum(_) => {}
    }
}

fn collect_expression(
    graph: &PackageGraph,
    expression: &TypeExpression,
    result: &mut BTreeSet<TypeRef>,
) {
    match expression {
        TypeExpression::Named(reference) => collect_type(graph, reference, result),
        TypeExpression::List(item, _)
        | TypeExpression::Map(item)
        | TypeExpression::Nullable(item)
        | TypeExpression::CursorPage(item) => collect_expression(graph, item, result),
        TypeExpression::Primitive(_, _) | TypeExpression::CursorQuery => {}
    }
}

fn action_references(action: &ActionDefinition) -> Vec<&TypeRef> {
    match action {
        ActionDefinition::Rpc { input, output, .. }
        | ActionDefinition::Live {
            input,
            event: output,
        } => vec![input, output],
        ActionDefinition::Operation {
            input,
            output,
            progress,
            update,
            signals,
            ..
        } => std::iter::once(input)
            .chain(std::iter::once(output))
            .chain(progress)
            .chain(update)
            .chain(signals.values())
            .collect(),
        ActionDefinition::Event { payload, .. } => vec![payload],
    }
}

fn find_api<'a>(
    graph: &'a PackageGraph,
    id: &trellis_idl::ApiId,
) -> Result<&'a trellis_idl::ApiDefinition, CodegenTsError> {
    graph
        .packages()
        .values()
        .find_map(|package| package.apis().get(id))
        .ok_or_else(|| CodegenTsError::MissingReference(id.to_string()))
}

fn type_ref(reference: &TypeRef, _package: &str, modules: &BTreeMap<String, String>) -> String {
    let module = module_alias(&modules[reference.package.as_str()]);
    format!("{module}.{}", reference.id)
}

fn type_codec(reference: &TypeRef, _package: &str, modules: &BTreeMap<String, String>) -> String {
    format!(
        "{}.{}Codec",
        module_alias(&modules[reference.package.as_str()]),
        reference.id
    )
}

fn descriptor_name(id: &ActionId) -> String {
    format!(
        "{}:{}",
        match id.kind {
            ActionKind::Rpc => "rpc",
            ActionKind::Operation => "operation",
            ActionKind::Event => "event",
            ActionKind::Live => "live",
        },
        id.name
    )
}

fn action_type_base(id: &ActionId) -> String {
    pascal(&id.name)
}

fn type_union(types: Vec<String>) -> String {
    if types.is_empty() {
        "never".to_owned()
    } else {
        types.join(" | ")
    }
}

fn api_modules(graph: &PackageGraph) -> BTreeMap<String, (String, String)> {
    let apis = graph
        .packages()
        .values()
        .flat_map(|package| package.apis().values())
        .collect::<Vec<_>>();
    let mut counts = BTreeMap::<String, usize>::new();
    for api in &apis {
        *counts.entry(api.name().to_lowercase()).or_default() += 1;
    }
    apis.into_iter()
        .map(|api| {
            let export = if counts[&api.name().to_lowercase()] == 1 {
                api.name().to_owned()
            } else {
                pascal(api.identity().as_str())
            };
            let module = if counts[&api.name().to_lowercase()] == 1 {
                api.name().to_owned()
            } else {
                export.clone()
            };
            (api.identity().as_str().to_owned(), (export, module))
        })
        .collect()
}

fn pascal(value: &str) -> String {
    value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| {
            let mut chars = token.chars();
            chars
                .next()
                .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
                .unwrap_or_default()
        })
        .collect()
}

fn property_name(value: &str) -> String {
    if is_safe_js_ident(value) {
        value.into()
    } else {
        js_string(value)
    }
}

fn module_alias(value: &str) -> String {
    format!("Types{}", value.trim_start_matches('p'))
}

fn participant_kind(value: ParticipantKind) -> &'static str {
    match value {
        ParticipantKind::Service => "service",
        ParticipantKind::Device => "device",
    }
}

fn direction(value: InteractionDirection) -> &'static str {
    match value {
        InteractionDirection::Call => "call",
        InteractionDirection::Invoke => "invoke",
        InteractionDirection::Publish => "publish",
        InteractionDirection::Subscribe => "subscribe",
    }
}

fn availability(optional: bool) -> &'static str {
    if optional {
        "\"optional\""
    } else {
        "\"required\""
    }
}

fn option_number(value: Option<u64>) -> String {
    value.map_or_else(|| "undefined".into(), |value| value.to_string())
}

fn idl_error(error: impl std::fmt::Debug) -> CodegenTsError {
    CodegenTsError::Idl(format!("{error:?}"))
}

fn is_safe_js_ident(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|character| character == '_' || character == '$' || character.is_alphabetic())
        && chars
            .all(|character| character == '_' || character == '$' || character.is_alphanumeric())
        && !matches!(
            value,
            "break"
                | "case"
                | "class"
                | "const"
                | "continue"
                | "debugger"
                | "default"
                | "delete"
                | "do"
                | "else"
                | "export"
                | "extends"
                | "finally"
                | "for"
                | "function"
                | "if"
                | "import"
                | "in"
                | "instanceof"
                | "new"
                | "return"
                | "super"
                | "switch"
                | "this"
                | "throw"
                | "try"
                | "typeof"
                | "var"
                | "void"
                | "while"
                | "with"
                | "yield"
        )
}

fn js_string(value: &str) -> String {
    serde_json::to_string(value).expect("JavaScript string")
}

/// Validate source paths and write a package into a caller-owned staging directory.
pub fn write_ts_sdk_sources(
    out_dir: &Path,
    sources: &[GeneratedTsSource],
) -> Result<(), CodegenTsError> {
    for source in sources {
        if source.path.is_absolute()
            || source
                .path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(CodegenTsError::InvalidOutputPath(source.path.clone()));
        }
    }
    for source in sources {
        write_generated_file(&out_dir.join(&source.path), &source.contents)?;
    }
    Ok(())
}

fn write_generated_file(path: &Path, contents: &str) -> Result<(), CodegenTsError> {
    let contents = format!("{}\n", contents.trim_end());
    if path.extension().is_some_and(|extension| extension == "ts") {
        validate_typescript(path, &contents)?;
    }
    write_if_changed(path, &contents)
}

fn validate_typescript(path: &Path, contents: &str) -> Result<(), CodegenTsError> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, contents, SourceType::ts()).parse();
    if parsed.diagnostics.is_empty() {
        return Ok(());
    }
    Err(CodegenTsError::InvalidTypeScript {
        path: path.to_path_buf(),
        message: parsed
            .diagnostics
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; "),
    })
}

fn emit_typescript(source: &GeneratedTsSource) -> Result<[GeneratedTsSource; 2], CodegenTsError> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, &source.contents, SourceType::ts()).parse();
    let mut program = parsed.program;
    let mut errors = parsed.diagnostics;
    if errors.is_empty() {
        let mut executable_source = source.contents.clone();
        for literal in program
            .body
            .iter()
            .rev()
            .filter_map(|statement| {
                statement
                    .as_module_declaration()
                    .and_then(|module| module.source())
            })
            .filter(|literal| literal.value.starts_with("./") || literal.value.starts_with("../"))
        {
            if let Some(stem) = literal.value.strip_suffix(".ts") {
                executable_source.replace_range(
                    literal.span.start as usize..literal.span.end as usize,
                    &js_string(&format!("{stem}.js")),
                );
            }
        }
        let executable_source = allocator.alloc_str(&executable_source);
        let executable = Parser::new(&allocator, executable_source, SourceType::ts()).parse();
        errors.extend(executable.diagnostics);
        program = executable.program;
        let declarations =
            IsolatedDeclarations::new(&allocator, IsolatedDeclarationsOptions::default())
                .build(&program);
        errors.extend(declarations.diagnostics);
        let declaration_source = Codegen::new().build(&declarations.program).code;
        let semantic = SemanticBuilder::new().build(&program);
        errors.extend(semantic.diagnostics);
        if errors.is_empty() {
            let transformed =
                Transformer::new(&allocator, &source.path, &TransformOptions::default())
                    .build_with_scoping(semantic.semantic.into_scoping(), &mut program);
            errors.extend(transformed.diagnostics);
            if errors.is_empty() {
                return Ok([
                    GeneratedTsSource {
                        path: source.path.with_extension("js"),
                        contents: format!(
                            "// Generated by Trellis. Do not edit.\n// @ts-self-types=\"./{}\"\n{}",
                            source
                                .path
                                .with_extension("d.ts")
                                .file_name()
                                .expect("generated module filename")
                                .to_string_lossy(),
                            Codegen::new().build(&program).code
                        ),
                    },
                    GeneratedTsSource {
                        path: source.path.with_extension("d.ts"),
                        contents: format!(
                            "// Generated by Trellis. Do not edit.\n{declaration_source}"
                        ),
                    },
                ]);
            }
        }
    }
    Err(CodegenTsError::InvalidTypeScript {
        path: source.path.clone(),
        message: errors
            .into_iter()
            .map(|error| format!("{:?}", error.with_source_code(source.contents.clone())))
            .collect::<Vec<_>>()
            .join("\n"),
    })
}

fn write_if_changed(path: &Path, contents: &str) -> Result<(), CodegenTsError> {
    if fs::read_to_string(path).ok().as_deref() == Some(contents) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use trellis_idl::project::{GenerateConfig, PackageManifest, PackageMetadata};
    use trellis_idl::SourceUnit;

    fn graph(source: &str) -> PackageGraph {
        trellis_idl::compile_project(
            &PackageManifest {
                package: PackageMetadata {
                    name: "example".into(),
                    version: "2.3.4".parse().unwrap(),
                },
                sources: BTreeMap::from([("main".into(), "contract.trellis".into())]),
                dependencies: BTreeMap::new(),
                generate: GenerateConfig::default(),
                default_registry: None,
                registries: BTreeMap::new(),
            },
            vec![SourceUnit {
                alias: "main".into(),
                path: "contract.trellis".into(),
                source: source.into(),
            }],
            BTreeMap::new(),
        )
        .unwrap()
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let parent = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.local/codegen-ts");
        fs::create_dir_all(&parent).unwrap();
        tempfile::Builder::new()
            .prefix(label)
            .tempdir_in(parent)
            .unwrap()
            .keep()
    }

    #[test]
    fn generated_typescript_package_typechecks() {
        let graph = graph(
            r#"
            type Count = uint64;
            type Blob = bytes;
            type When = timestamp;
            enum Status { ready; "roles.manage"; }
            model Node { id: ulid; next?: Node; data: Blob; count: Count; when: When; status: Status; }
            model Empty {}
            model PrivateV1 { value: string; }
            model PrivateState { entries: list<Node>; lookup: map<Node>; maybe: Node | null; }
            api orders@v1 {
              title "Orders";
              description "Order operations.";
              error Failed(Node);
              rpc Get { input Node; output Node; errors [Failed]; }
              operation Work { input Empty; output Node; progress Node; signals { Wake Empty; } upload; }
              event Changed { payload Node; }
              capabilities {
                capability read { consent_revision 1; title "Read"; description "Read orders."; consequence "Reveals order data."; allows { rpc Get; } }
                capability work { consent_revision 1; title "Work"; description "Run order work."; consequence "Changes orders."; allows { operation Work; } }
                capability observe { consent_revision 1; title "Observe"; description "Observe order changes."; consequence "Reveals order updates."; allows { subscribe event Changed; } }
              }
            }
            api catalog@v2 {
              title "Catalog";
              description "Catalog operations.";
              rpc List { input Empty; output Node; }
              capabilities { public { consent_revision 1; allows { rpc List; } } }
            }
            service Worker { implements orders; implements catalog; kv optional cache { title "Cache"; description "Cache"; schema PrivateState; version 2; accepts { 1: PrivateV1; } } job work { title "Work"; description "Work queue."; payload Empty; deadline 45s; retry { attempts 3; backoff [5s, 30s]; } } }
            app Viewer { use orders { required capability read; capability work; capability observe; } }
            device Sensor { app optional Console { use orders { required capability read; capability work; } state values { title "Values"; description "Values"; schema Node; } } }
            "#,
        );
        let root = unique_temp_dir("native");
        generate_ts_package(&graph, &root, "@example/generated").unwrap();
        fs::write(root.join("consumer.ts"), r#"
import { apps } from "./index.js";
import { codecs } from "@oatscenter/trellis/generated";
import type { AppRequestDescriptor } from "@oatscenter/trellis/generated";

const requested: AppRequestDescriptor = apps.Viewer.request;
const codec = apps.Sensor_Console.request.states.values.codec;
const decoded = codec.decode(JSON.parse(JSON.stringify(codec.encode({
  id: codecs.ulid.decode("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
  data: new Uint8Array([0, 127, 255]),
  count: 18446744073709551615n,
  when: codecs.timestamp.decode("2030-01-01T00:00:00Z"),
  status: "roles.manage",
}))));
if (decoded.count !== 18446744073709551615n || decoded.data[2] !== 255 || decoded.when !== "2030-01-01T00:00:00Z" || decoded.status !== "roles.manage") {
  throw new Error("Generated app-local State codec corrupted the value");
}
export { requested, decoded };
"#).unwrap();

        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let output = std::process::Command::new("deno")
            .args(["check", "--no-lock", "-c"])
            .arg(repo.join("ts/deno.json"))
            .arg(root.join("consumer.ts"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = std::process::Command::new("deno")
            .args(["run", "--no-lock", "-A", "-c"])
            .arg(repo.join("ts/deno.json"))
            .arg(root.join("consumer.ts"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn generated_types_split_progress_and_update_channels() {
        let graph = graph(
            r#"
            model Status { stage: string; }
            model Preview { text: string; }
            model Empty {}
            api work@v1 {
              title "Work";
              description "Work operations.";
              operation Run { input Empty; output Empty; progress Status; update Preview; }
              operation Step { input Empty; output Empty; progress Status; }
               capabilities { public { consent_revision 1; allows { operation Run; operation Step; } } }
            }
            service Worker { implements work; }
            "#,
        );
        let root = unique_temp_dir("channels");
        generate_ts_package(&graph, &root, "@example/generated").unwrap();
        fs::write(
            root.join("consumer.ts"),
            r#"
import type { RunProgress, RunUpdate, StepProgress, StepUpdate } from "./apis/work/mod.js";

const progress: RunProgress = { stage: "running" };
const update: RunUpdate = { text: "partial" };
const fallbackProgress: StepProgress = { stage: "running" };
const fallbackUpdate: StepUpdate = { stage: "running" };
// @ts-expect-error the update channel has its own schema, not the progress schema
const wrongProgress: RunProgress = { text: "partial" };
// @ts-expect-error the progress channel is not the update schema
const wrongUpdate: RunUpdate = { stage: "running" };

export { progress, update, fallbackProgress, fallbackUpdate, wrongProgress, wrongUpdate };
"#,
        )
        .unwrap();
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let output = std::process::Command::new("deno")
            .args(["check", "--no-lock", "-c"])
            .arg(repo.join("ts/deno.json"))
            .arg(root.join("consumer.ts"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn source_writer_rejects_paths_outside_package() {
        let root = unique_temp_dir("invalid-output-path");
        let error = write_ts_sdk_sources(
            &root.join("sdk"),
            &[GeneratedTsSource {
                path: PathBuf::from("../escape.ts"),
                contents: "export {};\n".to_string(),
            }],
        )
        .unwrap_err();
        assert!(matches!(error, CodegenTsError::InvalidOutputPath(_)));
        assert!(!root.join("escape.ts").exists());
    }

    #[test]
    fn invalid_generated_ts_is_rejected_before_write() {
        let root = unique_temp_dir("invalid-ts-before-write");
        let target = root.join("out/broken.ts");
        let error = write_generated_file(&target, "export const broken = ;\n").unwrap_err();
        assert!(matches!(error, CodegenTsError::InvalidTypeScript { .. }));
        assert!(!target.exists());
    }
}
