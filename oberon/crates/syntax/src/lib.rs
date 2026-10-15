//! Oberon-07 concrete syntax: the hand-written lexer, the LALRPOP grammar and
//! the syntax AST (DESIGN §2, §3, §4, §5, §6).
//!
//! This is the only crate that knows LALRPOP exists: the grammar, its `build.rs`
//! and the generated parsers stay here, and every phase above consumes a plain
//! syntax AST. The crate also carries the dialect switch (`Report` vs `Lower`)
//! and the diagnostics a parse can raise, because a lexical or syntax error is
//! the lowest phase's own.

pub mod ast;
pub mod diag;
pub mod lexer;

// LALRPOP's generated parser takes `&mut Vec<ErrorRecovery<..>>` for the
// recovery hook, which is its API, not a choice of ours: `clippy::ptr_arg`
// fires 244 times inside the generated file and nowhere in ours (PLAN.md W0.1).
lalrpop_util::lalrpop_mod!(#[allow(clippy::ptr_arg)] pub oberon);

use ast::{Module, TypeExpr};
use lexer::{Lexer, LexicalError, Token};
use lalrpop_util::ErrorRecovery;

pub use lalrpop_util::ParseError;

pub type ParseErr<'input> = ParseError<usize, Token<'input>, LexicalError>;

/// Which dialect the front end accepts (DESIGN §5: a dialect is a *gated* grammar,
/// not a second grammar). `Report` is the report's Oberon-07. `Lower` additionally
/// accepts reserved words spelled in lower case (PLAN.md W8.10) — the one extension
/// that cannot be additive, because the report makes capital and lower-case letters
/// distinct (report §3), so a report program that used `Begin` as a name would
/// tokenise differently.
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

    /// A note for a token that is a reserved word *only* because this dialect folds
    /// keyword case: without it a reader sees "unexpected `BEGIN`" where the program
    /// wrote `begin`, and has no way to tell why. The spelling is checked, so an
    /// honestly upper-case `BEGIN` — a reserved word in either dialect — gets no note.
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

/// Parse a module, recovering at statement level (DESIGN §5 D7): the module
/// comes back with the statements that parsed, together with every syntax error
/// the parser recovered from. Nothing is discarded - a recovered statement
/// becomes an empty one, and each error carries the input it dropped.
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
