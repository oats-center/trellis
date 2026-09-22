//! Native semantic Trellis package model.
use semver::Version;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
    path::PathBuf,
};

macro_rules! identifier {
    ($name:ident) => {
        #[doc = concat!("Typed semantic identifier `", stringify!($name), "`.")]
        #[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);

        impl $name {
            /// Return the canonical identifier text.
            pub fn as_str(&self) -> &str {
                &self.0
            }
            pub(crate) fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

identifier!(PackageId);
identifier!(TypeId);
identifier!(ApiId);
identifier!(ParticipantId);
identifier!(CapabilityId);
identifier!(ResourceName);

/// A source unit supplied to the pure compiler.
#[derive(Clone, Debug)]
pub struct SourceUnit {
    /// Manifest source alias.
    pub alias: String,
    /// Normalized package-relative path used in diagnostics only.
    pub path: PathBuf,
    /// UTF-8 Trellis source.
    pub source: String,
}

/// A source location retained separately from semantic values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceSpan {
    /// Normalized source path.
    pub path: PathBuf,
    /// Byte range within the UTF-8 source.
    pub range: Range<usize>,
}

/// Source locations keyed by stable semantic paths.
pub type SourceMap = BTreeMap<String, SourceSpan>;

/// A resolved immutable package closure.
#[derive(Clone, Debug)]
pub struct PackageGraph {
    pub(crate) root: PackageId,
    pub(crate) packages: BTreeMap<PackageId, SemanticPackage>,
    pub(crate) digests: BTreeMap<PackageId, String>,
    pub(crate) participant_needs: BTreeMap<ParticipantId, ParticipantNeeds>,
}

impl PackageGraph {
    /// Return the root package identity.
    pub fn root(&self) -> &PackageId {
        &self.root
    }
    /// Return every package in deterministic identity order.
    pub fn packages(&self) -> &BTreeMap<PackageId, SemanticPackage> {
        &self.packages
    }
    /// Return one package by identity.
    pub fn package(&self, id: &PackageId) -> Option<&SemanticPackage> {
        self.packages.get(id)
    }
    /// Return the semantic digest for one package.
    pub fn digest(&self, id: &PackageId) -> Option<&str> {
        self.digests.get(id).map(String::as_str)
    }
    /// Return the root semantic package.
    pub fn root_package(&self) -> &SemanticPackage {
        &self.packages[&self.root]
    }
    /// Return the root package digest.
    pub fn root_digest(&self) -> &str {
        &self.digests[&self.root]
    }

    /// Return the exact derived needs for one participant.
    pub fn participant_needs(&self, id: &ParticipantId) -> Option<&ParticipantNeeds> {
        self.participant_needs.get(id)
    }

    /// Project validator schemas for every wire channel of one action.
    ///
    /// # Errors
    ///
    /// Returns an error when the API/action is absent or a referenced type cannot be projected.
    pub fn action_codecs(
        &self,
        api: &ApiId,
        action: &ActionId,
    ) -> miette::Result<ActionCodecProjection> {
        let api = self
            .api(api)
            .ok_or_else(|| miette::miette!("API is absent from package graph"))?
            .definition;
        let action = api
            .actions
            .get(action)
            .ok_or_else(|| miette::miette!("action is absent from API"))?;
        let schema = |reference: &TypeRef| crate::json_schema(self, reference);
        let errors = |names: &BTreeSet<String>| {
            names
                .iter()
                .filter_map(|name| api.errors[name].as_ref().map(|ty| (name, ty)))
                .map(|(name, ty)| Ok((name.clone(), schema(ty)?)))
                .collect::<miette::Result<_>>()
        };
        Ok(match action {
            ActionDefinition::Rpc {
                input,
                output,
                errors: names,
                ..
            } => ActionCodecProjection {
                input: Some(schema(input)?),
                output: Some(schema(output)?),
                errors: errors(names)?,
                ..Default::default()
            },
            ActionDefinition::Operation {
                input,
                output,
                update,
                errors: names,
                signals,
                ..
            } => ActionCodecProjection {
                input: Some(schema(input)?),
                output: Some(schema(output)?),
                update: update.as_ref().map(schema).transpose()?,
                signals: signals
                    .iter()
                    .map(|(name, ty)| Ok((name.clone(), schema(ty)?)))
                    .collect::<miette::Result<_>>()?,
                errors: errors(names)?,
                ..Default::default()
            },
            ActionDefinition::Event { payload, .. } => ActionCodecProjection {
                payload: Some(schema(payload)?),
                ..Default::default()
            },
            ActionDefinition::Feed { input, event } => ActionCodecProjection {
                input: Some(schema(input)?),
                payload: Some(schema(event)?),
                ..Default::default()
            },
        })
    }

    /// Resolve one API together with the graph arena that owns its type references.
    pub fn api(&self, id: &ApiId) -> Option<ResolvedApi<'_>> {
        self.packages.values().find_map(|package| {
            package.apis.get(id).map(|definition| ResolvedApi {
                graph: self,
                definition,
            })
        })
    }

    /// Resolve one participant resource with the graph arena owning its schemas.
    pub fn resource(
        &self,
        participant: &ParticipantId,
        name: &ResourceName,
    ) -> Option<ResolvedResource<'_>> {
        self.packages.values().find_map(|package| {
            package
                .participants
                .get(participant)
                .and_then(|participant| participant.resources.get(name))
                .map(|definition| ResolvedResource {
                    graph: self,
                    definition,
                })
        })
    }
}

/// An API definition paired with the immutable graph arena needed to resolve its schemas.
#[derive(Clone, Copy)]
pub struct ResolvedApi<'a> {
    pub(crate) graph: &'a PackageGraph,
    pub(crate) definition: &'a ApiDefinition,
}

impl ResolvedApi<'_> {
    /// Return the API definition.
    pub fn definition(&self) -> &ApiDefinition {
        self.definition
    }
}

/// A resource declaration paired with the immutable graph arena owning its schemas.
#[derive(Clone, Copy)]
pub struct ResolvedResource<'a> {
    pub(crate) graph: &'a PackageGraph,
    pub(crate) definition: &'a ResourceDefinition,
}

impl ResolvedResource<'_> {
    /// Return the resource definition.
    pub fn definition(&self) -> &ResourceDefinition {
        self.definition
    }
}

/// One package in a resolved graph.
#[derive(Clone, Debug)]
pub struct SemanticPackage {
    pub(crate) identity: PackageId,
    pub(crate) version: Version,
    pub(crate) dependencies: BTreeMap<PackageId, ResolvedDependency>,
    pub(crate) types: BTreeMap<TypeId, TypeDefinition>,
    pub(crate) apis: BTreeMap<ApiId, ApiDefinition>,
    pub(crate) participants: BTreeMap<ParticipantId, ParticipantDefinition>,
    pub(crate) sources: SourceMap,
}

impl SemanticPackage {
    /// Return the package identity.
    pub fn identity(&self) -> &PackageId {
        &self.identity
    }
    /// Return the authored package version.
    pub fn version(&self) -> &Version {
        &self.version
    }
    /// Return exact direct dependencies keyed by package identity.
    pub fn dependencies(&self) -> &BTreeMap<PackageId, ResolvedDependency> {
        &self.dependencies
    }
    /// Return package types.
    pub fn types(&self) -> &BTreeMap<TypeId, TypeDefinition> {
        &self.types
    }
    /// Return package APIs.
    pub fn apis(&self) -> &BTreeMap<ApiId, ApiDefinition> {
        &self.apis
    }
    /// Return package participants.
    pub fn participants(&self) -> &BTreeMap<ParticipantId, ParticipantDefinition> {
        &self.participants
    }
    /// Return source locations, excluded from semantic identity.
    pub fn source_map(&self) -> &SourceMap {
        &self.sources
    }
}

/// Exact direct package dependency.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedDependency {
    /// Exact package version.
    pub version: Version,
    /// Exact semantic package digest.
    pub digest: String,
}

/// Reference to a type in the package graph arena.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TypeRef {
    /// Owning package.
    pub package: PackageId,
    /// Package-local type name.
    pub id: TypeId,
}

/// Primitive Trellis value kinds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Primitive {
    /// UTF-8 string.
    String,
    /// Boolean.
    Bool,
    /// Signed 32-bit integer encoded as a JSON number.
    Int32,
    /// Unsigned 32-bit integer encoded as a JSON number.
    Uint32,
    /// Signed 64-bit integer encoded as a canonical decimal string.
    Int64,
    /// Unsigned 64-bit integer encoded as a canonical decimal string.
    Uint64,
    /// Finite floating-point number.
    Number,
    /// Bytes encoded as padded standard base64.
    Bytes,
    /// Canonical UTC RFC3339 timestamp.
    Timestamp,
    /// Canonical uppercase ULID.
    Ulid,
}

/// Scalar or collection bounds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bounds {
    /// Inclusive numeric minimum.
    pub min: Option<NumericBound>,
    /// Inclusive numeric maximum.
    pub max: Option<NumericBound>,
    /// Minimum string length or list item count.
    pub min_count: Option<u64>,
    /// Maximum string length or list item count.
    pub max_count: Option<u64>,
}

/// An exact integer bound or a finite floating-point bound.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NumericBound {
    /// Exact integer bound.
    Integer(i128),
    /// Finite floating-point bound.
    Number(f64),
}

impl NumericBound {
    pub(crate) fn compare(self, other: Self) -> Option<std::cmp::Ordering> {
        match self {
            Self::Integer(value) => match other {
                Self::Integer(other) => Some(value.cmp(&other)),
                Self::Number(other) => (value as f64).partial_cmp(&other),
            },
            Self::Number(value) => match other {
                Self::Integer(other) => value.partial_cmp(&(other as f64)),
                Self::Number(other) => value.partial_cmp(&other),
            },
        }
    }
}

/// A resolved Trellis type expression.
#[derive(Clone, Debug, PartialEq)]
pub enum TypeExpression {
    /// Primitive with its scalar constraints.
    Primitive(Primitive, Bounds),
    /// Nominal package type reference.
    Named(TypeRef),
    /// Ordered list and list-count constraints.
    List(Box<TypeExpression>, Bounds),
    /// String-keyed map.
    Map(Box<TypeExpression>),
    /// Explicitly nullable value.
    Nullable(Box<TypeExpression>),
    /// Reserved cursor request object.
    CursorQuery,
    /// Reserved cursor response object.
    CursorPage(Box<TypeExpression>),
}

/// Package-level type declaration.
#[derive(Clone, Debug, PartialEq)]
pub enum TypeDefinition {
    /// Open model keyed by lower-camel field name.
    Model(BTreeMap<String, ModelField>),
    /// Open symbolic enum with known generated symbols.
    Enum(BTreeSet<String>),
    /// Nominal alias or named scalar.
    Alias(TypeExpression),
}

/// One open-model field.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelField {
    /// Resolved field type.
    pub ty: TypeExpression,
    /// Whether the field may be absent.
    pub optional: bool,
    /// Whether this direct model edge belongs to a recursive strongly connected component.
    pub recursive: bool,
}

/// Human-facing text excluded where canonical semantic rules require.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Documentation {
    /// Human-facing title.
    pub title: String,
    /// Human-facing description.
    pub description: String,
}

/// One API and its typed actions.
#[derive(Clone, Debug)]
pub struct ApiDefinition {
    pub(crate) identity: ApiId,
    pub(crate) name: String,
    pub(crate) major: u32,
    pub(crate) version: Option<Version>,
    pub(crate) docs: Documentation,
    pub(crate) errors: BTreeMap<String, Option<TypeRef>>,
    pub(crate) actions: BTreeMap<ActionId, ActionDefinition>,
    pub(crate) capabilities: BTreeMap<CapabilityId, CapabilityDefinition>,
    pub(crate) subjects: trellis_protocol::DerivedApiSubjects,
}

impl ApiDefinition {
    /// Return the qualified API identity.
    pub fn identity(&self) -> &ApiId {
        &self.identity
    }
    /// Return the package-local API name.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Return the transport major.
    pub fn major(&self) -> u32 {
        self.major
    }
    /// Return optional presentation release metadata.
    pub fn version(&self) -> Option<&Version> {
        self.version.as_ref()
    }
    /// Return presentation documentation.
    pub fn documentation(&self) -> &Documentation {
        &self.docs
    }
    /// Return API-scoped named errors.
    pub fn errors(&self) -> &BTreeMap<String, Option<TypeRef>> {
        &self.errors
    }
    /// Return typed API actions.
    pub fn actions(&self) -> &BTreeMap<ActionId, ActionDefinition> {
        &self.actions
    }
    /// Return API capabilities.
    pub fn capabilities(&self) -> &BTreeMap<CapabilityId, CapabilityDefinition> {
        &self.capabilities
    }

    /// Return protocol-derived subjects for every API action.
    pub fn subjects(&self) -> &trellis_protocol::DerivedApiSubjects {
        &self.subjects
    }
}

/// Native validator projections for an action's wire channels.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActionCodecProjection {
    /// Request or invocation input schema.
    pub input: Option<Value>,
    /// RPC or operation result schema.
    pub output: Option<Value>,
    /// Event/feed item schema.
    pub payload: Option<Value>,
    /// Operation progress schema.
    pub update: Option<Value>,
    /// Operation signal schemas.
    pub signals: BTreeMap<String, Value>,
    /// Known domain-error payload schemas.
    pub errors: BTreeMap<String, Value>,
}

/// Exact API action identity.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ActionId {
    /// Action category.
    pub kind: ActionKind,
    /// Exact authored action path.
    pub name: String,
}

/// API action kind.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ActionKind {
    /// Request/response RPC.
    Rpc,
    /// Durable operation.
    Operation,
    /// Published event.
    Event,
    /// Streaming feed.
    Feed,
}

/// Typed API action.
#[derive(Clone, Debug)]
pub enum ActionDefinition {
    /// Request/response RPC definition.
    Rpc {
        /// Request schema.
        input: TypeRef,
        /// Response schema.
        output: TypeRef,
        /// Declared API-scoped domain errors.
        errors: BTreeSet<String>,
        /// Whether responses may carry a download grant.
        download: bool,
        /// Optional native pagination marker.
        pagination: Option<Pagination>,
    },
    /// Durable operation definition.
    Operation {
        /// Invocation schema.
        input: TypeRef,
        /// Completion schema.
        output: TypeRef,
        /// Optional progress/update schema.
        update: Option<TypeRef>,
        /// Declared API-scoped domain errors.
        errors: BTreeSet<String>,
        /// Named signal input schemas.
        signals: BTreeMap<String, TypeRef>,
        /// Whether invocation accepts an upload grant.
        upload: bool,
    },
    /// Event definition.
    Event {
        /// Event payload schema.
        payload: TypeRef,
        /// Ordered typed payload paths used as routing parameters.
        parameters: Vec<Vec<String>>,
    },
    /// Feed definition.
    Feed {
        /// Feed request schema.
        input: TypeRef,
        /// Stream item schema.
        event: TypeRef,
    },
}

/// Native pagination marker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Pagination {
    /// Cursor request/page protocol.
    Cursor,
}

/// Capability policy and consent text.
#[derive(Clone, Debug)]
pub struct CapabilityDefinition {
    /// Presentation title.
    pub title: String,
    /// Consent description.
    pub description: String,
    /// Consent consequence.
    pub consequence: String,
    /// Exact covered actions.
    pub allows: BTreeSet<ActionSelection>,
    /// Whether the capability is public.
    pub public: bool,
}

/// A selected action and direction.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ActionSelection {
    /// Selected action identity.
    pub action: ActionId,
    /// Selected interaction direction.
    pub direction: InteractionDirection,
}

/// Interaction direction, explicit for events.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum InteractionDirection {
    /// Invoke an RPC.
    Call,
    /// Invoke an Operation and its lifecycle routes.
    Invoke,
    /// Publish an Event.
    Publish,
    /// Subscribe to an Event or Feed.
    Subscribe,
}

/// A participant in a package.
#[derive(Clone, Debug)]
pub struct ParticipantDefinition {
    pub(crate) identity: ParticipantId,
    pub(crate) name: String,
    pub(crate) kind: ParticipantKind,
    pub(crate) implements: BTreeSet<ApiId>,
    pub(crate) uses: BTreeMap<ApiId, InteractionSelection>,
    pub(crate) resources: BTreeMap<ResourceName, ResourceDefinition>,
    pub(crate) companion: Option<CompanionDefinition>,
}

impl ParticipantDefinition {
    /// Return the qualified participant identity.
    pub fn identity(&self) -> &ParticipantId {
        &self.identity
    }
    /// Return its lexical name.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Return its participant kind.
    pub fn kind(&self) -> ParticipantKind {
        self.kind
    }
    /// Return wholly implemented APIs.
    pub fn implements(&self) -> &BTreeSet<ApiId> {
        &self.implements
    }
    /// Return exact selected interactions.
    pub fn uses(&self) -> &BTreeMap<ApiId, InteractionSelection> {
        &self.uses
    }
    /// Return participant resources.
    pub fn resources(&self) -> &BTreeMap<ResourceName, ResourceDefinition> {
        &self.resources
    }
    /// Return the optional device companion.
    pub fn companion(&self) -> Option<&CompanionDefinition> {
        self.companion.as_ref()
    }
}

/// Participant kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParticipantKind {
    /// Service process.
    Service,
    /// Native device process.
    Device,
    /// User application.
    App,
    /// User agent.
    Agent,
}

/// Exact interaction selections for one API.
#[derive(Clone, Debug)]
pub struct InteractionSelection {
    /// API containing the selected actions.
    pub api: ApiId,
    /// Selected API actions.
    pub actions: BTreeSet<ActionSelection>,
    /// Implicated capabilities explicitly allowed to be unavailable.
    pub optional_capabilities: BTreeSet<CapabilityId>,
}

/// Exact participant authority needs derived from selected interactions and resources.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParticipantNeeds {
    pub(crate) digest: String,
    pub(crate) required_grants: trellis_protocol::GrantSet,
    pub(crate) optional_grants: BTreeMap<String, trellis_protocol::GrantSet>,
    pub(crate) required_capabilities: BTreeSet<CapabilityId>,
}

impl ParticipantNeeds {
    /// Return the semantic needs digest.
    pub fn digest(&self) -> &str {
        &self.digest
    }
    /// Return exact grants required for participant readiness.
    pub fn required_grants(&self) -> &trellis_protocol::GrantSet {
        &self.required_grants
    }
    /// Return exact optional grant bundles keyed by capability or resource identity.
    pub fn optional_grants(&self) -> &BTreeMap<String, trellis_protocol::GrantSet> {
        &self.optional_grants
    }
    /// Return implicated named capabilities required for readiness.
    pub fn required_capabilities(&self) -> &BTreeSet<CapabilityId> {
        &self.required_capabilities
    }
}

/// A device's single nested app or agent.
#[derive(Clone, Debug)]
pub struct CompanionDefinition {
    /// Nested participant identity.
    pub participant: ParticipantId,
    /// Whether the companion may be unavailable.
    pub optional: bool,
}

/// Retry behavior.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetryPolicy {
    /// Total delivery attempts.
    pub attempts: u32,
    /// Delay after each failed nonfinal attempt, in milliseconds.
    pub backoff_ms: Vec<u64>,
}

/// Queue key policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyConcurrencyPolicy {
    /// Queue work sharing the key.
    Queue,
    /// Reject concurrent work sharing the key.
    Reject,
    /// Supersede older work sharing the key.
    Supersede,
}

/// Compiled queue key path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyConcurrency {
    /// Ordered required scalar payload path.
    pub path: Vec<String>,
    /// Concurrency policy for equal extracted keys.
    pub policy: KeyConcurrencyPolicy,
}

/// Historic wire representation.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoricRepresentation {
    /// Positive historical representation version.
    pub version: u32,
    /// Historical schema decoded before application migration.
    pub ty: TypeRef,
}

/// Consumer replay starting position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Replay {
    /// Consume only events published after consumer creation.
    New,
    /// Consume all retained events.
    All,
}

/// Kind-specific participant resource.
#[derive(Clone, Debug)]
pub enum ResourceDefinition {
    /// Single typed State value.
    State {
        /// Whether resource availability is optional.
        optional: bool,
        /// Presentation metadata.
        docs: Documentation,
        /// Current value schema.
        schema: TypeRef,
        /// Current representation version.
        version: u32,
        /// Explicit historical representations accepted by application migration handlers.
        accepts: Vec<HistoricRepresentation>,
    },
    /// Typed versioned key-value resource.
    Kv {
        /// Whether resource availability is optional.
        optional: bool,
        /// Presentation metadata.
        docs: Documentation,
        /// Current value schema.
        schema: TypeRef,
        /// Current representation version.
        version: u32,
        /// Explicit historical representations accepted by application migration handlers.
        accepts: Vec<HistoricRepresentation>,
        /// Desired retained value revisions per key.
        history: u64,
        /// Desired retention in milliseconds, where zero means forever.
        ttl_ms: u64,
        /// Optional desired maximum value size in bytes.
        desired_max_value: Option<u64>,
    },
    /// Raw object Store resource.
    Store {
        /// Whether resource availability is optional.
        optional: bool,
        /// Presentation metadata.
        docs: Documentation,
        /// Desired retention in milliseconds, where zero means forever.
        ttl_ms: u64,
        /// Optional desired maximum object size in bytes.
        desired_max_object: Option<u64>,
        /// Optional desired maximum total size in bytes.
        desired_max_total: Option<u64>,
    },
    /// Private typed Job queue.
    Job {
        /// Whether resource availability is optional.
        optional: bool,
        /// Presentation metadata.
        docs: Documentation,
        /// Job payload schema.
        payload: TypeRef,
        /// Optional completion schema.
        result: Option<TypeRef>,
        /// Optional progress/update schema.
        update: Option<TypeRef>,
        /// Optional creation-relative deadline in milliseconds.
        deadline_ms: Option<u64>,
        /// Optional authored retry override.
        retry: Option<RetryPolicy>,
        /// Optional typed key concurrency policy.
        key_concurrency: Option<KeyConcurrency>,
    },
    /// Explicit durable Event consumer.
    Consumer {
        /// Whether resource availability is optional.
        optional: bool,
        /// Presentation metadata.
        docs: Documentation,
        /// Exact selected Event identities.
        events: BTreeSet<(ApiId, String)>,
        /// Local handler concurrency.
        concurrency: u32,
        /// Initial replay position.
        replay: Replay,
        /// Optional authored retry override.
        retry: Option<RetryPolicy>,
    },
}

impl ResourceDefinition {
    /// Return whether the resource may be absent at runtime.
    pub fn optional(&self) -> bool {
        match self {
            Self::State { optional, .. }
            | Self::Kv { optional, .. }
            | Self::Store { optional, .. }
            | Self::Job { optional, .. }
            | Self::Consumer { optional, .. } => *optional,
        }
    }
}
