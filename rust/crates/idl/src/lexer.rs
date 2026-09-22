use logos::Logos;
use std::ops::Range;

#[derive(Logos, Clone, Debug, PartialEq)]
#[logos(skip r"[ \t\r\n\f]+")]
#[logos(skip(r"//[^\n]*", allow_greedy = true))]
#[logos(skip r"(?s:/\*.*?\*/)")]
pub(crate) enum TokenKind {
    #[regex(r"[A-Za-z_][A-Za-z0-9_]*")]
    Ident,
    #[regex(r"-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?")]
    Number,
    #[regex(r#"\"([^\"\\]|\\.)*\""#)]
    String,
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token("[")]
    LBracket,
    #[token("]")]
    RBracket,
    #[token("<")]
    LAngle,
    #[token(">")]
    RAngle,
    #[token(":")]
    Colon,
    #[token(";")]
    Semi,
    #[token("=")]
    Eq,
    #[token(",")]
    Comma,
    #[token("?")]
    Question,
    #[token("|")]
    Pipe,
    #[token(".")]
    Dot,
    #[token("@")]
    At,
}

#[derive(Clone, Debug)]
pub(crate) struct Token {
    pub kind: TokenKind,
    pub span: Range<usize>,
}

pub(crate) fn lex(source: &str) -> Result<Vec<Token>, Range<usize>> {
    let mut lexer = TokenKind::lexer(source);
    let mut tokens = Vec::new();
    while let Some(token) = lexer.next() {
        let span = lexer.span();
        tokens.push(Token {
            kind: token.map_err(|()| span.clone())?,
            span,
        });
    }
    Ok(tokens)
}
