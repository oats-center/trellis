use crate::{
    ast::{
        Action, Alias, Api, ApiUse, Capability, Declaration, Enum, Field, Import, MemberValue,
        Model, ParsedSource, Participant, Prelude, PreludeDependency, Resource, ResourceValue,
        Selection, Spanned, TypeExpr,
    },
    lexer::{lex, Token, TokenKind},
    semantic::SourceUnit,
};
use miette::{LabeledSpan, MietteDiagnostic, NamedSource, Report};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn parse(sources: &[SourceUnit]) -> miette::Result<Vec<ParsedSource>> {
    sources
        .iter()
        .enumerate()
        .map(|(index, source)| Parser::new(source, index)?.source())
        .collect()
}

struct Parser<'a> {
    input: &'a SourceUnit,
    source_index: usize,
    tokens: Vec<Token>,
    position: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a SourceUnit, source_index: usize) -> miette::Result<Self> {
        let tokens = lex(&input.source)
            .map_err(|span| diagnostic(input, span, "unrecognized token in Trellis IDL"))?;
        Ok(Self {
            input,
            source_index,
            tokens,
            position: 0,
        })
    }

    fn source(mut self) -> miette::Result<ParsedSource> {
        let mut prelude = Prelude::default();
        let mut imports = BTreeMap::new();
        let mut declarations = Vec::new();
        if self.at_word("package") {
            self.word("package")?;
            prelude.package = Some(self.string()?);
            self.token(TokenKind::Semi)?;
        }
        while self.at_word("dependency") {
            self.word("dependency")?;
            let alias = self.ident()?;
            let package = self.string()?;
            self.word("version")?;
            let version = self.string()?;
            self.word("digest")?;
            let digest = self.string()?;
            self.token(TokenKind::Semi)?;
            prelude.dependencies.push(PreludeDependency {
                alias,
                package,
                version,
                digest,
            });
        }
        while self.at_word("import") {
            self.word("import")?;
            self.token(TokenKind::LBrace)?;
            while !self.at(TokenKind::RBrace) {
                let start = self.current_span().start;
                let name = self.ident()?;
                let local = if self.eat_word("as") {
                    self.ident()?
                } else {
                    name.clone()
                };
                if imports
                    .insert(
                        local.clone(),
                        Import {
                            from: String::new(),
                            name,
                            span: start..self.previous_span().end,
                        },
                    )
                    .is_some()
                {
                    return Err(self.error_here(format!("duplicate import name '{local}'")));
                }
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.token(TokenKind::RBrace)?;
            self.word("from")?;
            let from = self.ident()?;
            self.token(TokenKind::Semi)?;
            for import in imports.values_mut().filter(|import| import.from.is_empty()) {
                import.from.clone_from(&from);
            }
        }
        while !self.done() {
            let start = self.current_span().start;
            let declaration = match self.word_text()?.as_str() {
                "model" => Declaration::Model(self.model()?),
                "enum" => Declaration::Enum(self.enum_decl()?),
                "type" => Declaration::Alias(self.alias()?),
                "api" => Declaration::Api(self.api()?),
                kind @ ("service" | "device" | "app" | "agent") => {
                    Declaration::Participant(self.participant(kind.to_owned(), false)?)
                }
                other => {
                    return Err(self.error_previous(format!("unsupported declaration '{other}'")))
                }
            };
            let end = self.previous_span().end;
            declarations.push(Spanned {
                value: declaration,
                source: self.source_index,
                span: start..end,
            });
        }
        Ok(ParsedSource {
            alias: self.input.alias.clone(),
            path: self.input.path.clone(),
            text: self.input.source.clone(),
            imports,
            declarations,
            prelude,
        })
    }

    fn model(&mut self) -> miette::Result<Model> {
        let name = self.ident()?;
        self.token(TokenKind::LBrace)?;
        let mut fields = BTreeMap::new();
        while !self.at(TokenKind::RBrace) {
            let field = self.ident()?;
            let optional = self.eat(TokenKind::Question);
            self.token(TokenKind::Colon)?;
            let ty = self.ty()?;
            self.token(TokenKind::Semi)?;
            if fields
                .insert(field.clone(), Field { optional, ty })
                .is_some()
            {
                return Err(self.error_previous(format!("duplicate field '{field}'")));
            }
        }
        self.token(TokenKind::RBrace)?;
        Ok(Model { name, fields })
    }

    fn enum_decl(&mut self) -> miette::Result<Enum> {
        let name = self.ident()?;
        self.token(TokenKind::LBrace)?;
        let mut symbols = Vec::new();
        while !self.at(TokenKind::RBrace) {
            let symbol = self.ident()?;
            if symbols.contains(&symbol) {
                return Err(self.error_previous(format!("duplicate enum symbol '{symbol}'")));
            }
            symbols.push(symbol);
            self.token(TokenKind::Semi)?;
        }
        self.token(TokenKind::RBrace)?;
        Ok(Enum { name, symbols })
    }

    fn alias(&mut self) -> miette::Result<Alias> {
        let name = self.ident()?;
        self.token(TokenKind::Eq)?;
        let ty = self.ty()?;
        self.token(TokenKind::Semi)?;
        Ok(Alias { name, ty })
    }

    fn ty(&mut self) -> miette::Result<TypeExpr> {
        let mut ty = self.type_member()?;
        if self.eat(TokenKind::Pipe) {
            self.word("null")?;
            if self.eat(TokenKind::Pipe) {
                return Err(self.error_previous("only 'T | null' unions are supported"));
            }
            ty = TypeExpr::Nullable(Box::new(ty));
        }
        Ok(ty)
    }

    fn type_member(&mut self) -> miette::Result<TypeExpr> {
        let name = self.ident()?;
        match name.as_str() {
            "list" | "map" | "CursorPage" => {
                self.token(TokenKind::LAngle)?;
                let member = self.ty()?;
                self.token(TokenKind::RAngle)?;
                let constraints = self.constraints()?;
                match name.as_str() {
                    "list" => Ok(TypeExpr::List(Box::new(member), constraints)),
                    "map" if constraints.is_empty() => Ok(TypeExpr::Map(Box::new(member))),
                    "CursorPage" if constraints.is_empty() => {
                        Ok(TypeExpr::CursorPage(Box::new(member)))
                    }
                    _ => Err(self.error_previous(format!("{name} does not accept constraints"))),
                }
            }
            "string" | "bool" | "int32" | "uint32" | "int64" | "uint64" | "number" | "bytes"
            | "timestamp" | "ulid" => Ok(TypeExpr::Primitive(name, self.constraints()?)),
            "null" => Err(self.error_previous("'null' is only legal as 'T | null'")),
            _ => {
                if self.at(TokenKind::LParen) {
                    return Err(self.error_here("constraints require a scalar or list alias"));
                }
                Ok(TypeExpr::Named(name))
            }
        }
    }

    fn constraints(&mut self) -> miette::Result<Vec<(String, String)>> {
        if !self.eat(TokenKind::LParen) {
            return Ok(Vec::new());
        }
        let mut values = Vec::new();
        while !self.at(TokenKind::RParen) {
            let name = self.ident()?;
            self.token(TokenKind::Eq)?;
            let value = self.number_text()?;
            if values.iter().any(|(existing, _)| existing == &name) {
                return Err(self.error_previous(format!("duplicate constraint '{name}'")));
            }
            values.push((name, value));
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.token(TokenKind::RParen)?;
        Ok(values)
    }

    fn api(&mut self) -> miette::Result<Api> {
        let name = self.ident()?;
        self.token(TokenKind::At)?;
        let version_name = self.ident()?;
        let major = version_name
            .strip_prefix('v')
            .and_then(|value| value.parse().ok())
            .filter(|value| *value > 0)
            .ok_or_else(|| self.error_previous("expected API transport major 'vN'"))?;
        self.token(TokenKind::LBrace)?;
        let mut api = Api {
            name,
            major,
            title: String::new(),
            description: String::new(),
            version: None,
            errors: BTreeMap::new(),
            actions: Vec::new(),
            capabilities: Vec::new(),
            capabilities_present: false,
        };
        let mut members = BTreeSet::new();
        while !self.at(TokenKind::RBrace) {
            let member = self.word_text()?;
            match member.as_str() {
                "title" | "description" | "version" | "capabilities"
                    if !members.insert(member.clone()) =>
                {
                    return Err(self.error_previous(format!("duplicate API member '{member}'")));
                }
                "title" => api.title = self.string_statement()?,
                "description" => api.description = self.string_statement()?,
                "version" => api.version = Some(self.string_statement()?),
                "error" => {
                    let name = self.ident()?;
                    let payload = if self.eat(TokenKind::LParen) {
                        let value = self.name()?;
                        self.token(TokenKind::RParen)?;
                        Some(value)
                    } else {
                        None
                    };
                    self.token(TokenKind::Semi)?;
                    if api.errors.insert(name.clone(), payload).is_some() {
                        return Err(self.error_previous(format!("duplicate error '{name}'")));
                    }
                }
                kind @ ("rpc" | "operation" | "event" | "feed") => {
                    api.actions.push(self.action(kind.to_owned())?)
                }
                "capabilities" => {
                    api.capabilities = self.capabilities()?;
                    api.capabilities_present = true;
                }
                other => {
                    return Err(self.error_previous(format!("unsupported API member '{other}'")))
                }
            }
        }
        self.token(TokenKind::RBrace)?;
        Ok(api)
    }

    fn action(&mut self, kind: String) -> miette::Result<Action> {
        let start = self.previous_span().start;
        let name = self.path()?;
        self.token(TokenKind::LBrace)?;
        let mut members = BTreeMap::new();
        while !self.at(TokenKind::RBrace) {
            let member = self.word_text()?;
            let value = match member.as_str() {
                "input" | "output" | "payload" | "event" | "progress" => {
                    MemberValue::Name(self.name_statement()?)
                }
                "errors" => MemberValue::Names(self.name_list_statement()?),
                "params" => MemberValue::Paths(self.path_list_statement()?),
                "upload" | "download" => {
                    self.token(TokenKind::Semi)?;
                    MemberValue::Flag
                }
                "pagination" => {
                    let value = self.ident()?;
                    self.token(TokenKind::Semi)?;
                    MemberValue::Name(value)
                }
                "signals" => {
                    self.token(TokenKind::LBrace)?;
                    let mut signals = BTreeMap::new();
                    while !self.at(TokenKind::RBrace) {
                        let signal = self.ident()?;
                        let ty = self.name_statement()?;
                        if signals.insert(signal.clone(), ty).is_some() {
                            return Err(self.error_previous(format!("duplicate signal '{signal}'")));
                        }
                    }
                    self.token(TokenKind::RBrace)?;
                    MemberValue::Signals(signals)
                }
                other => {
                    return Err(self.error_previous(format!("unsupported {kind} member '{other}'")))
                }
            };
            if members.insert(member.clone(), value).is_some() {
                return Err(self.error_previous(format!("duplicate {kind} member '{member}'")));
            }
        }
        self.token(TokenKind::RBrace)?;
        let span = start..self.previous_span().end;
        Ok(Action {
            kind,
            name,
            members,
            span,
        })
    }

    fn capabilities(&mut self) -> miette::Result<Vec<Capability>> {
        self.token(TokenKind::LBrace)?;
        let mut values = Vec::new();
        while !self.at(TokenKind::RBrace) {
            let start = self.current_span().start;
            let head = self.word_text()?;
            let (public, name) = match head.as_str() {
                "public" => (true, "public".to_owned()),
                "capability" => (false, self.ident()?),
                _ => return Err(self.error_previous("expected 'public' or 'capability'")),
            };
            self.token(TokenKind::LBrace)?;
            let mut capability = Capability {
                name,
                public,
                title: String::new(),
                description: String::new(),
                consequence: String::new(),
                allows: Vec::new(),
                span: start..start,
            };
            let mut members = BTreeSet::new();
            while !self.at(TokenKind::RBrace) {
                let member = self.word_text()?;
                if !members.insert(member.clone()) {
                    return Err(
                        self.error_previous(format!("duplicate capability member '{member}'"))
                    );
                }
                match member.as_str() {
                    "title" => capability.title = self.string_statement()?,
                    "description" => capability.description = self.string_statement()?,
                    "consequence" => capability.consequence = self.string_statement()?,
                    "allows" => capability.allows = self.selection_block(false)?,
                    other => {
                        return Err(
                            self.error_previous(format!("unsupported capability member '{other}'"))
                        )
                    }
                }
            }
            self.token(TokenKind::RBrace)?;
            capability.span = start..self.previous_span().end;
            values.push(capability);
        }
        self.token(TokenKind::RBrace)?;
        Ok(values)
    }

    fn participant(&mut self, kind: String, optional: bool) -> miette::Result<Participant> {
        let start = self.previous_span().start;
        let name = self.ident()?;
        self.token(TokenKind::LBrace)?;
        let mut value = Participant {
            kind: kind.clone(),
            name,
            implements: Vec::new(),
            uses: Vec::new(),
            resources: Vec::new(),
            companion: None,
            optional,
            span: start..start,
        };
        while !self.at(TokenKind::RBrace) {
            let member = self.word_text()?;
            match member.as_str() {
                "implements" => value.implements.push(self.name_statement()?),
                "use" => {
                    let api = self.name()?;
                    let mut selections = Vec::new();
                    let mut optional_capabilities = Vec::new();
                    if self.eat(TokenKind::Semi) {
                    } else {
                        self.token(TokenKind::LBrace)?;
                        while !self.at(TokenKind::RBrace) {
                            if self.at_word("optional") {
                                self.word("optional")?;
                                self.word("capability")?;
                                optional_capabilities.push(self.name_statement()?);
                            } else {
                                selections.push(self.selection()?);
                            }
                        }
                        self.token(TokenKind::RBrace)?;
                    }
                    value.uses.push(ApiUse {
                        api,
                        selections,
                        optional_capabilities,
                    });
                }
                kind @ ("state" | "kv" | "store" | "job" | "consumer") => {
                    let start = self.previous_span().start;
                    let optional = self.eat_word("optional");
                    let mut resource = self.resource(kind.to_owned(), optional)?;
                    resource.span.start = start;
                    value.resources.push(resource)
                }
                "app" | "agent" if kind == "device" => {
                    let start = self.previous_span().start;
                    if value.companion.is_some() {
                        return Err(self.error_previous("a device may contain only one companion"));
                    }
                    let optional = self.eat_word("optional");
                    let mut companion = self.participant(member, optional)?;
                    companion.span.start = start;
                    value.companion = Some(Box::new(companion));
                }
                "optional" => {
                    return Err(self.error_previous(
                        "resource optionality follows its kind, for example 'kv optional cache'",
                    ))
                }
                other => {
                    return Err(
                        self.error_previous(format!("unsupported participant member '{other}'"))
                    )
                }
            }
        }
        self.token(TokenKind::RBrace)?;
        value.span = start..self.previous_span().end;
        Ok(value)
    }

    fn resource(&mut self, kind: String, optional: bool) -> miette::Result<Resource> {
        let start = self.previous_span().start;
        let name = self.ident()?;
        self.token(TokenKind::LBrace)?;
        let mut members = BTreeMap::new();
        while !self.at(TokenKind::RBrace) {
            let member = self.word_text()?;
            let value = match member.as_str() {
                "title" | "description" => ResourceValue::Text(self.string_statement()?),
                "schema" | "payload" | "result" | "update" => {
                    ResourceValue::Name(self.name_statement()?)
                }
                "version" | "history" | "concurrency" => {
                    ResourceValue::Integer(self.integer_statement()?)
                }
                "ttl" => ResourceValue::Duration(self.duration_statement(true)?),
                "deadline" => ResourceValue::Duration(self.duration_statement(false)?),
                "desired_max_value" | "desired_max_object" | "desired_max_total" => {
                    ResourceValue::Capacity(self.capacity_statement()?)
                }
                "events" => ResourceValue::Names(self.name_list_statement()?),
                "accepts" => ResourceValue::Accepts(self.accepts()?),
                "retry" => self.retry()?,
                "key_concurrency" => self.key_concurrency()?,
                "replay" => ResourceValue::Name(self.name_statement()?),
                other => {
                    return Err(self
                        .error_previous(format!("unsupported {kind} resource member '{other}'")))
                }
            };
            if members.insert(member.clone(), value).is_some() {
                return Err(self.error_previous(format!("duplicate resource member '{member}'")));
            }
        }
        self.token(TokenKind::RBrace)?;
        let span = start..self.previous_span().end;
        Ok(Resource {
            kind,
            name,
            optional,
            members,
            span,
        })
    }

    fn accepts(&mut self) -> miette::Result<Vec<(u32, String)>> {
        self.token(TokenKind::LBrace)?;
        let mut values = Vec::new();
        while !self.at(TokenKind::RBrace) {
            let version = self.integer()?;
            let version = u32::try_from(version)
                .map_err(|_| self.error_previous("representation version exceeds u32"))?;
            self.token(TokenKind::Colon)?;
            let ty = self.name_statement()?;
            values.push((version, ty));
        }
        self.token(TokenKind::RBrace)?;
        Ok(values)
    }

    fn retry(&mut self) -> miette::Result<ResourceValue> {
        self.token(TokenKind::LBrace)?;
        self.word("attempts")?;
        let attempts = u32::try_from(self.integer_statement()?)
            .map_err(|_| self.error_previous("retry attempts exceeds u32"))?;
        self.word("backoff")?;
        self.token(TokenKind::LBracket)?;
        let mut backoff_ms = Vec::new();
        while !self.at(TokenKind::RBracket) {
            backoff_ms.push(self.duration(false)?);
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.token(TokenKind::RBracket)?;
        self.token(TokenKind::Semi)?;
        self.token(TokenKind::RBrace)?;
        Ok(ResourceValue::Retry {
            attempts,
            backoff_ms,
        })
    }

    fn key_concurrency(&mut self) -> miette::Result<ResourceValue> {
        self.token(TokenKind::LBrace)?;
        self.word("path")?;
        let path = self
            .name_statement()?
            .split('.')
            .map(str::to_owned)
            .collect();
        self.word("policy")?;
        let policy = self.name_statement()?;
        self.token(TokenKind::RBrace)?;
        Ok(ResourceValue::KeyConcurrency { path, policy })
    }

    fn selection_block(&mut self, opened: bool) -> miette::Result<Vec<Selection>> {
        if !opened {
            self.token(TokenKind::LBrace)?;
        }
        let mut values = Vec::new();
        while !self.at(TokenKind::RBrace) {
            values.push(self.selection()?);
        }
        self.token(TokenKind::RBrace)?;
        Ok(values)
    }

    fn selection(&mut self) -> miette::Result<Selection> {
        let first = self.ident()?;
        let (direction, kind) = match first.as_str() {
            "rpc" => ("call".to_owned(), "rpc".to_owned()),
            "operation" => ("invoke".to_owned(), "operation".to_owned()),
            "feed" => ("subscribe".to_owned(), "feed".to_owned()),
            "publish" | "subscribe" => {
                let kind = self.ident()?;
                if kind != "event" {
                    return Err(self.error_previous("publish/subscribe requires 'event'"));
                }
                (first, kind)
            }
            _ => {
                return Err(self.error_previous(
                    "expected rpc, operation, feed, publish event, or subscribe event",
                ))
            }
        };
        let name = self.path()?;
        self.token(TokenKind::Semi)?;
        Ok(Selection {
            direction,
            kind,
            name,
        })
    }

    fn name_list_statement(&mut self) -> miette::Result<Vec<String>> {
        self.token(TokenKind::LBracket)?;
        let mut values = Vec::new();
        while !self.at(TokenKind::RBracket) {
            values.push(self.name()?);
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.token(TokenKind::RBracket)?;
        self.token(TokenKind::Semi)?;
        Ok(values)
    }

    fn path_list_statement(&mut self) -> miette::Result<Vec<Vec<String>>> {
        self.token(TokenKind::LBracket)?;
        let mut values = Vec::new();
        while !self.at(TokenKind::RBracket) {
            values.push(self.path()?.split('.').map(str::to_owned).collect());
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.token(TokenKind::RBracket)?;
        self.token(TokenKind::Semi)?;
        Ok(values)
    }

    fn name_statement(&mut self) -> miette::Result<String> {
        let value = self.name()?;
        self.token(TokenKind::Semi)?;
        Ok(value)
    }
    fn string_statement(&mut self) -> miette::Result<String> {
        let value = self.string()?;
        self.token(TokenKind::Semi)?;
        Ok(value)
    }
    fn integer_statement(&mut self) -> miette::Result<u64> {
        let value = self.integer()?;
        self.token(TokenKind::Semi)?;
        Ok(value)
    }
    fn duration_statement(&mut self, zero_bare: bool) -> miette::Result<u64> {
        let value = self.duration(zero_bare)?;
        self.token(TokenKind::Semi)?;
        Ok(value)
    }
    fn capacity_statement(&mut self) -> miette::Result<u64> {
        let value = self.quantity(false)?;
        self.token(TokenKind::Semi)?;
        Ok(value)
    }

    fn integer(&mut self) -> miette::Result<u64> {
        let text = self.number_text()?;
        text.parse()
            .map_err(|_| self.error_previous("expected an unsigned integer"))
    }

    fn duration(&mut self, zero_bare: bool) -> miette::Result<u64> {
        let value = self.quantity(true)?;
        if value == 0 && !zero_bare && self.previous_kind() == Some(TokenKind::Number) {
            return Err(self.error_previous("duration requires ms, s, m, h, or d"));
        }
        Ok(value)
    }

    fn quantity(&mut self, duration: bool) -> miette::Result<u64> {
        let number = self.integer()?;
        let unit = if self.at(TokenKind::Ident) {
            Some(self.ident()?)
        } else {
            None
        };
        let multiplier = match (duration, unit.as_deref()) {
            (true, Some("ms")) => 1,
            (true, Some("s")) => 1_000,
            (true, Some("m")) => 60_000,
            (true, Some("h")) => 3_600_000,
            (true, Some("d")) => 86_400_000,
            (true, None) if number == 0 => 1,
            (false, Some("B")) => 1,
            (false, Some("KiB")) => 1 << 10,
            (false, Some("MiB")) => 1 << 20,
            (false, Some("GiB")) => 1 << 30,
            _ => {
                return Err(self.error_previous(if duration {
                    "expected duration unit ms, s, m, h, or d"
                } else {
                    "expected capacity unit B, KiB, MiB, or GiB"
                }))
            }
        };
        number
            .checked_mul(multiplier)
            .ok_or_else(|| self.error_previous("quantity exceeds u64"))
    }

    fn name(&mut self) -> miette::Result<String> {
        self.path()
    }
    fn path(&mut self) -> miette::Result<String> {
        let mut value = self.ident()?;
        while self.eat(TokenKind::Dot) {
            value.push('.');
            value.push_str(&self.ident()?);
        }
        Ok(value)
    }

    fn string(&mut self) -> miette::Result<String> {
        let span = self.token(TokenKind::String)?;
        serde_json::from_str(self.text(&span))
            .map_err(|error| self.error_at(span, format!("invalid string: {error}")))
    }
    fn ident(&mut self) -> miette::Result<String> {
        let span = self.token(TokenKind::Ident)?;
        Ok(self.text(&span).to_owned())
    }
    fn number_text(&mut self) -> miette::Result<String> {
        let span = self.token(TokenKind::Number)?;
        Ok(self.text(&span).to_owned())
    }
    fn word(&mut self, expected: &str) -> miette::Result<()> {
        let value = self.ident()?;
        if value != expected {
            return Err(self.error_previous(format!("expected '{expected}'")));
        }
        Ok(())
    }
    fn word_text(&mut self) -> miette::Result<String> {
        self.ident()
    }
    fn eat_word(&mut self, word: &str) -> bool {
        if self.at_word(word) {
            self.position += 1;
            true
        } else {
            false
        }
    }
    fn at_word(&self, word: &str) -> bool {
        self.tokens
            .get(self.position)
            .is_some_and(|token| token.kind == TokenKind::Ident && self.text(&token.span) == word)
    }
    fn token(&mut self, expected: TokenKind) -> miette::Result<std::ops::Range<usize>> {
        let Some(token) = self.tokens.get(self.position) else {
            return Err(self.error_here(format!("expected {}", token_name(&expected))));
        };
        if token.kind != expected {
            return Err(self.error_here(format!("expected {}", token_name(&expected))));
        }
        self.position += 1;
        Ok(token.span.clone())
    }
    fn eat(&mut self, kind: TokenKind) -> bool {
        if self.at(kind) {
            self.position += 1;
            true
        } else {
            false
        }
    }
    fn at(&self, kind: TokenKind) -> bool {
        self.tokens
            .get(self.position)
            .is_some_and(|token| token.kind == kind)
    }
    fn done(&self) -> bool {
        self.position == self.tokens.len()
    }
    fn text(&self, span: &std::ops::Range<usize>) -> &str {
        &self.input.source[span.clone()]
    }
    fn current_span(&self) -> std::ops::Range<usize> {
        self.tokens
            .get(self.position)
            .map(|token| token.span.clone())
            .unwrap_or(self.input.source.len()..self.input.source.len())
    }
    fn previous_span(&self) -> std::ops::Range<usize> {
        self.tokens
            .get(self.position.saturating_sub(1))
            .map(|token| token.span.clone())
            .unwrap_or(0..0)
    }
    fn previous_kind(&self) -> Option<TokenKind> {
        self.tokens
            .get(self.position.saturating_sub(1))
            .map(|token| token.kind.clone())
    }
    fn error_here(&self, message: impl Into<String>) -> Report {
        diagnostic(self.input, self.current_span(), message)
    }
    fn error_previous(&self, message: impl Into<String>) -> Report {
        diagnostic(self.input, self.previous_span(), message)
    }
    fn error_at(&self, span: std::ops::Range<usize>, message: impl Into<String>) -> Report {
        diagnostic(self.input, span, message)
    }
}

pub(crate) fn diagnostic(
    source: &SourceUnit,
    span: std::ops::Range<usize>,
    message: impl Into<String>,
) -> Report {
    Report::new(
        MietteDiagnostic::new(message.into()).with_labels(vec![LabeledSpan::underline((
            span.start,
            span.len().max(1),
        ))]),
    )
    .with_source_code(NamedSource::new(
        source.path.display().to_string(),
        source.source.clone(),
    ))
}

fn token_name(kind: &TokenKind) -> &'static str {
    match kind {
        TokenKind::Ident => "an identifier",
        TokenKind::Number => "a numeric literal",
        TokenKind::String => "a quoted string",
        TokenKind::LBrace => "'{'",
        TokenKind::RBrace => "'}'",
        TokenKind::LParen => "'('",
        TokenKind::RParen => "')'",
        TokenKind::LBracket => "'['",
        TokenKind::RBracket => "']'",
        TokenKind::LAngle => "'<'",
        TokenKind::RAngle => "'>'",
        TokenKind::Colon => "':'",
        TokenKind::Semi => "';'",
        TokenKind::Eq => "'='",
        TokenKind::Comma => "','",
        TokenKind::Question => "'?'",
        TokenKind::Pipe => "'|'",
        TokenKind::Dot => "'.'",
        TokenKind::At => "'@'",
    }
}
