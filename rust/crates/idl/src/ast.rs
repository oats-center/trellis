use std::{collections::BTreeMap, ops::Range, path::PathBuf};

#[derive(Clone, Debug)]
pub(crate) struct Spanned<T> {
    pub value: T,
    pub source: usize,
    pub span: Range<usize>,
}

#[derive(Clone, Debug)]
pub(crate) struct ParsedSource {
    pub alias: String,
    pub path: PathBuf,
    pub text: String,
    pub imports: BTreeMap<String, Import>,
    pub declarations: Vec<Spanned<Declaration>>,
    pub prelude: Prelude,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Prelude {
    pub package: Option<String>,
    pub dependencies: Vec<PreludeDependency>,
}

#[derive(Clone, Debug)]
pub(crate) struct PreludeDependency {
    pub alias: String,
    pub package: String,
    pub version: String,
    pub digest: String,
}

#[derive(Clone, Debug)]
pub(crate) struct Import {
    pub from: String,
    pub name: String,
    pub span: std::ops::Range<usize>,
}

#[derive(Clone, Debug)]
pub(crate) enum Declaration {
    Model(Model),
    Enum(Enum),
    Alias(Alias),
    Api(Api),
    Participant(Participant),
}

#[derive(Clone, Debug)]
pub(crate) struct Model {
    pub name: String,
    pub fields: BTreeMap<String, Field>,
}
#[derive(Clone, Debug)]
pub(crate) struct Field {
    pub optional: bool,
    pub ty: TypeExpr,
}
#[derive(Clone, Debug)]
pub(crate) struct Enum {
    pub name: String,
    pub symbols: Vec<String>,
}
#[derive(Clone, Debug)]
pub(crate) struct Alias {
    pub name: String,
    pub ty: TypeExpr,
}

#[derive(Clone, Debug)]
pub(crate) enum TypeExpr {
    Named(String),
    Primitive(String, Vec<(String, String)>),
    List(Box<TypeExpr>, Vec<(String, String)>),
    Map(Box<TypeExpr>),
    Nullable(Box<TypeExpr>),
    CursorPage(Box<TypeExpr>),
}

#[derive(Clone, Debug)]
pub(crate) struct Api {
    pub name: String,
    pub major: u32,
    pub title: String,
    pub description: String,
    pub version: Option<String>,
    pub errors: BTreeMap<String, Option<String>>,
    pub actions: Vec<Action>,
    pub capabilities: Vec<Capability>,
    pub capabilities_present: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Action {
    pub kind: String,
    pub name: String,
    pub members: BTreeMap<String, MemberValue>,
    pub span: Range<usize>,
}
#[derive(Clone, Debug)]
pub(crate) enum MemberValue {
    Name(String),
    Names(Vec<String>),
    Flag,
    Paths(Vec<Vec<String>>),
    Signals(BTreeMap<String, String>),
}

#[derive(Clone, Debug)]
pub(crate) struct Capability {
    pub name: String,
    pub public: bool,
    pub title: String,
    pub description: String,
    pub consequence: String,
    pub allows: Vec<Selection>,
    pub span: Range<usize>,
}

#[derive(Clone, Debug)]
pub(crate) struct Selection {
    pub direction: String,
    pub kind: String,
    pub name: String,
}

#[derive(Clone, Debug)]
pub(crate) struct Participant {
    pub kind: String,
    pub name: String,
    pub implements: Vec<String>,
    pub uses: Vec<ApiUse>,
    pub resources: Vec<Resource>,
    pub companion: Option<Box<Participant>>,
    pub optional: bool,
    pub span: Range<usize>,
}

#[derive(Clone, Debug)]
pub(crate) struct ApiUse {
    pub api: String,
    pub selections: Vec<Selection>,
    pub optional_capabilities: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct Resource {
    pub kind: String,
    pub name: String,
    pub optional: bool,
    pub members: BTreeMap<String, ResourceValue>,
    pub span: Range<usize>,
}

#[derive(Clone, Debug)]
pub(crate) enum ResourceValue {
    Text(String),
    Name(String),
    Integer(u64),
    Duration(u64),
    Capacity(u64),
    Names(Vec<String>),
    Accepts(Vec<(u32, String)>),
    Retry { attempts: u32, backoff_ms: Vec<u64> },
    KeyConcurrency { path: Vec<String>, policy: String },
}
