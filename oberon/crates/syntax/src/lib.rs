//! Oberon-07 concrete syntax: the hand-written lexer, the LALRPOP grammar and
//! the syntax AST (DESIGN 2, 3, 4, 5, 6).
//!
//! This is the only crate that knows LALRPOP exists. Its grammar, `build.rs`
//! and generated parsers stay here. Every phase above consumes a plain syntax
//! AST. The crate also contains the dialect switch (`Report` vs `Lower`) and
//! diagnostics for lexical and syntax errors.

pub mod ast;
pub mod diag;
pub mod lexer;

// LALRPOP's generated parser takes `&mut Vec<ErrorRecovery<..>>` for the
// recovery hook, which is its API, not a choice of ours: `clippy::ptr_arg`
// fires 244 times inside the generated file and nowhere in ours (PLAN.md W0.1).
#[path = "oberon.rs"]
#[allow(clippy::ptr_arg)]
pub mod oberon;

use ast::{Module, TypeExpr};
use lexer::{Lexer, LexicalError, Token};
use lalrpop_util::ErrorRecovery;

pub use lalrpop_util::ParseError;

pub type ParseErr<'input> = ParseError<usize, Token<'input>, LexicalError>;

/// The front end uses one of two dialects (DESIGN 5). Each is a *gated* grammar,
/// not a separate grammar. `Report` is report Oberon-07. `Lower` accepts reserved
/// words in lower case too (PLAN.md W8.10). This extension changes parsing because
/// the report distinguishes capital and lower-case letters (report section 3).
/// A report program using `Begin` as a name would tokenize differently.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum Dialect {
    #[default]
    Report,
    Lower,
}

impl Dialect {
    /// The name the interface header records, and the driver accepts.
    pub fn name(self) -> &'static str {
        match self {
            Dialect::Report => "report",
            Dialect::Lower => "lower",
        }
    }

    pub fn from_name(name: &str) -> Option<Dialect> {
        match name {
            "report" => Some(Dialect::Report),
            "lower" => Some(Dialect::Lower),
            _ => None,
        }
    }

    fn lexer<'input>(self, input: &'input str) -> Lexer<'input> {
        match self {
            Dialect::Report => Lexer::new(input),
            Dialect::Lower => Lexer::with_folded_keywords(input),
        }
    }

    /// Explain why a token is reserved only in this dialect, which folds keyword
    /// case. Without the note, a reader sees "unexpected `BEGIN`" where the
    /// program wrote `begin`. The spelling is checked, so upper-case `BEGIN`
    /// (reserved in either dialect) doesn't need one.
    fn collision_note(self, tok: &Token<'_>, text: &str) -> Option<String> {
        if self != Dialect::Lower {
            return None;
        }
        let spelling = lexer::keyword_spelling(tok)?;
        if text == spelling {
            return None;
        }
        Some(format!(
            "this dialect folds reserved-word case, so `{text}` is the reserved word \
             `{spelling}` here; an identifier needs a spelling that is not a reserved word \
             in any case (W8.10)"
        ))
    }
}

pub fn parse_module<'input>(input: &'input str) -> Result<Module, ParseErr<'input>> {
    parse_module_dialect(input, Dialect::Report)
}

pub fn parse_module_dialect<'input>(
    input: &'input str,
    dialect: Dialect,
) -> Result<Module, ParseErr<'input>> {
    let mut errors = Vec::new();
    let module = oberon::CompilationUnitParser::new().parse(&mut errors, dialect.lexer(input))?;
    match errors.into_iter().next() {
        // The grammar recovers so that one run can report every syntax error;
        // this entry point keeps the fail-fast contract its callers expect.
        Some(first) => Err(first.error),
        None => Ok(module),
    }
}

/// Parse a module, recovering at statement level (DESIGN 5 D7). The module
/// includes the statements that parsed and all syntax errors the parser recovered
/// from. A recovered statement becomes empty, and each error records the input
/// that was dropped.
pub fn parse_module_recovering<'input>(
    input: &'input str,
) -> (
    Option<Module>,
    Vec<ErrorRecovery<usize, Token<'input>, LexicalError>>,
) {
    parse_module_recovering_dialect(input, Dialect::Report)
}

pub fn parse_module_recovering_dialect<'input>(
    input: &'input str,
    dialect: Dialect,
) -> (
    Option<Module>,
    Vec<ErrorRecovery<usize, Token<'input>, LexicalError>>,
) {
    let mut errors = Vec::new();
    match oberon::CompilationUnitParser::new().parse(&mut errors, dialect.lexer(input)) {
        Ok(module) => (Some(module), errors),
        Err(_) => (None, errors),
    }
}

/// Parse a type as an interface file renders it, using the module grammar. The
/// inverse of the interface writer: an `Item::Type` from a `.sym` file is a
/// string, and this reads it back into the syntax AST the checker understands.
pub fn rendered_type(text: &str) -> Option<TypeExpr> {
    let mut lexer = Lexer::new(text);
    let mut errors = Vec::new();
    match oberon::RenderedTypeParser::new().parse(&mut errors, &mut lexer) {
        // An interface rendering is either readable or it is not; there is no
        // recovery inside one type.
        Ok(t) if errors.is_empty() => Some(t),
        _ => None,
    }
}
