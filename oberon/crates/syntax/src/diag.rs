//! Diagnostics: the located messages a front end raises, and the renderer that
//! turns them into `file:line:col: severity: message` (DESIGN 10).
//!
//! They live with the syntax phase because it raises lexical and parse errors.
//! The renderer needs only a span and the source text. The semantic pass and
//! back ends construct and consume the same type.

use std::fmt::Write as _;

use lalrpop_util::{ErrorRecovery, ParseError};

use crate::ast::{Module, Span};
use crate::lexer::{LexicalError, Token};
use crate::{Dialect, ParseErr};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub span: Span,
    pub message: String,
    /// A second line explains the compiler's expectation, the applicable report
    /// rule, or the likely meaning of the mistaken construct.
    pub notes: Vec<String>,
}

impl Diagnostic {
    pub fn new(severity: Severity, span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            severity,
            span,
            message: message.into(),
            notes: Vec::new(),
        }
    }

    pub fn error(span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic::new(Severity::Error, span, message)
    }

    /// Notes accumulate: a syntax error can have both "what would have fitted
    /// here" and "what the recovery discarded", and the reader needs both
    /// (DESIGN 10).
    pub fn with_note(mut self, note: impl Into<String>) -> Diagnostic {
        self.notes.push(note.into());
        self
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

/// Drop diagnostics that repeat one already reported: two passes over the same
/// mistake (a call looked up twice, say) must not read as two mistakes.
pub fn dedup_diagnostics(diags: &mut Vec<Diagnostic>) {
    let mut seen: Vec<(Severity, u32, u32, String)> = Vec::new();
    diags.retain(|d| {
        let key = (d.severity, d.span.lo, d.span.hi, d.message.clone());
        if seen.contains(&key) {
            false
        } else {
            seen.push(key);
            true
        }
    });
}

/// Convert a byte offset to `line:column` (1-based) (DESIGN 10). The AST stores
/// byte spans. The caller printing a diagnostic performs the conversion.
pub fn line_col(src: &str, offset: u32) -> (u32, u32) {
    let off = (offset as usize).min(src.len());
    let mut line = 1;
    let mut line_start = 0;
    for (i, b) in src.as_bytes()[..off].iter().enumerate() {
        if *b == b'\n' {
            line += 1;
            line_start = i + 1;
        }
    }
    (line, (off - line_start) as u32 + 1)
}

/// The source line containing an offset, without its line break.
fn source_line(src: &str, offset: u32) -> &str {
    let off = (offset as usize).min(src.len());
    let start = src[..off].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let end = src[off..].find('\n').map(|i| off + i).unwrap_or(src.len());
    &src[start..end]
}

fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
    }
}

/// Keeps at most `limit` errors (warnings are never dropped) and returns how
/// many were suppressed, so a caller can say so. `0` means no limit: a file with
/// sixty broken statements should not print sixty diagnostics, but below the
/// limit nothing changes at all (PLAN.md W5.2).
pub fn limit_errors(diags: &mut Vec<Diagnostic>, limit: usize) -> usize {
    if limit == 0 {
        return 0;
    }
    let mut seen = 0;
    let mut suppressed = 0;
    diags.retain(|d| {
        if !d.is_error() {
            return true;
        }
        seen += 1;
        if seen > limit {
            suppressed += 1;
            return false;
        }
        true
    });
    suppressed
}

/// The note for a recovered error: what the parser threw away to carry on.
/// Every dropped token is a token of the *input*, so nothing is filtered: a
/// program may legally declare `error` or `Eof` (report section 3), and omitting
/// either from the note would misdescribe what was skipped.
pub fn recovery_note(tokens: &[(usize, Token<'_>, usize)]) -> Option<String> {
    let text: Vec<String> = tokens.iter().map(|(_, t, _)| t.to_string()).collect();
    if text.is_empty() {
        return None;
    }
    const SHOWN: usize = 8;
    let shown = text.iter().take(SHOWN).cloned().collect::<Vec<_>>().join(" ");
    let more = text.len().saturating_sub(SHOWN);
    let mut note = format!("the parser skipped `{shown}` to carry on; a later error may be a consequence of this one");
    if more > 0 {
        let _ = write!(note, " ({more} more token(s) were skipped)");
    }
    Some(note)
}

/// A syntax error as a first-class diagnostic, so that a parse failure is
/// located and rendered just like a type error (DESIGN 5 D7, 10).
pub fn diagnostic_for_parse_error(error: &ParseErr<'_>) -> Diagnostic {
    use lalrpop_util::ParseError as P;
    let expected_note = |expected: &[String]| -> Option<String> {
        if expected.is_empty() {
            return None;
        }
        // LALRPOP names terminals with their own quoting; a reader wants the
        // tokens themselves.
        let clean = |s: &String| s.trim_matches('"').to_string();
        // A missing separator is common. Put `;` first when it would have fitted.
        let mut ordered: Vec<&String> = expected.iter().collect();
        if let Some(i) = ordered.iter().position(|s| clean(s) == ";") {
            let semi = ordered.remove(i);
            ordered.insert(0, semi);
        }
        const SHOWN: usize = 6;
        let mut shown: Vec<String> = ordered.iter().take(SHOWN).map(|s| clean(s)).collect();
        let more = expected.len().saturating_sub(SHOWN);
        let last = shown.pop();
        let mut note = match (shown.is_empty(), last) {
            (true, Some(only)) => format!("expected {only}"),
            (false, Some(last)) => format!("expected {} or {last}", shown.join(", ")),
            _ => return None,
        };
        if more > 0 {
            let _ = write!(note, " ({more} other token(s) would do as well)");
        }
        Some(note)
    };
    let d = match error {
        P::InvalidToken { location } => {
            Diagnostic::error(span_of(*location, *location + 1), "the source is not valid here")
        }
        P::UnrecognizedEof { location, expected } => {
            let mut d = Diagnostic::error(
                span_of(*location, *location),
                "unexpected end of file: something is missing",
            );
            if let Some(n) = expected_note(expected) {
                d = d.with_note(n);
            }
            d
        }
        P::UnrecognizedToken { token, expected } => {
            let (lo, tok, hi) = token;
            let mut d = Diagnostic::error(span_of(*lo, *hi), format!("unexpected `{tok}`"));
            if let Some(n) = expected_note(expected) {
                d = d.with_note(n);
            }
            d
        }
        P::ExtraToken { token } => {
            let (lo, tok, hi) = token;
            Diagnostic::error(
                span_of(*lo, *hi),
                format!("unexpected `{tok}` after the end of the module"),
            )
        }
        P::User { error } => {
            let (lo, hi) = error.span();
            Diagnostic::error(span_of(lo, hi), error.to_string()).with_note(error.note())
        }
    };
    d
}

pub fn span_of(lo: usize, hi: usize) -> Span {
    Span::new(lo, hi)
}

/// Render diagnostics the way a compiler should: `file:line:col: severity:
/// message`, then the offending source line, a caret under the span, and the
/// note.
pub fn render_diagnostics(path: &str, src: &str, diags: &[Diagnostic]) -> String {
    let mut out = String::new();
    for d in diags {
        let (line, col) = line_col(src, d.span.lo);
        let _ = writeln!(
            out,
            "{path}:{line}:{col}: {}: {}",
            severity_name(d.severity),
            d.message
        );
        let text = source_line(src, d.span.lo);
        if !text.trim().is_empty() {
            let width = line.to_string().len();
            let _ = writeln!(out, "{:width$} |", "");
            let _ = writeln!(out, "{line} | {text}");
            let (end_line, end_col) = line_col(src, d.span.hi);
            let carets = if end_line == line && end_col > col {
                (end_col - col) as usize
            } else {
                1
            };
            let pad = " ".repeat((col as usize - 1).min(text.len()));
            let _ = writeln!(out, "{:width$} | {pad}{}", "", "^".repeat(carets));
        }
        for note in &d.notes {
            let _ = writeln!(out, "  = note: {note}");
        }
    }
    out
}

/// Parse a module and turn every syntax error into a located diagnostic,
/// including a fatal error that recovery could not absorb (a missing `END` or a
/// lexical error). Return the module when the parser built one.
pub fn parse_with_diagnostics(input: &str) -> (Option<Module>, Vec<Diagnostic>) {
    parse_with_diagnostics_dialect(input, Dialect::Report)
}

pub fn parse_with_diagnostics_dialect(
    input: &str,
    dialect: Dialect,
) -> (Option<Module>, Vec<Diagnostic>) {
    let mut errors = Vec::new();
    let recovered = |errors: &Vec<ErrorRecovery<usize, Token<'_>, LexicalError>>| -> Vec<Diagnostic> {
        errors
            .iter()
            .map(|r| {
                let mut d = diagnostic_for_parse_error(&r.error);
                if let Some(n) = recovery_note(&r.dropped_tokens) {
                    d = d.with_note(n);
                }
                // This dialect folds case, so it recognizes a lower-case reserved
                // word (W8.10).
                if let ParseError::UnrecognizedToken { token: (start, tok, end), .. } = &r.error {
                    let text = input.get(*start..*end).unwrap_or("");
                    if let Some(n) = dialect.collision_note(tok, text) {
                        d = d.with_note(n);
                    }
                }
                d
            })
            .collect()
    };
    match crate::oberon::CompilationUnitParser::new().parse(&mut errors, dialect.lexer(input)) {
        Ok(module) => {
            let mut diags = recovered(&errors);
            // Statement-level recovery can still return a module with a broken
            // declaration list. Check declarations chunk by chunk to find errors
            // after the first one (PLAN.md W5.1). Clean parses skip this check.
            if diags.iter().any(Diagnostic::is_error) {
                diags.extend(declaration_diagnostics(input, dialect));
                dedup_diagnostics(&mut diags);
                diags.sort_by_key(|d| (d.span.lo, d.span.hi));
            }
            (Some(module), diags)
        }
        Err(fatal) => {
            let mut diags = recovered(&errors);
            diags.push(diagnostic_for_parse_error(&fatal));
            // The grammar cannot recover inside a declaration section because
            // each section may be empty. Parse declarations individually so a
            // missing `;` does not hide later errors (PLAN.md W5.1).
            diags.extend(declaration_diagnostics(input, dialect));
            dedup_diagnostics(&mut diags);
            diags.sort_by_key(|d| (d.span.lo, d.span.hi));
            (None, diags)
        }
    }
}

/// Find errors in each declaration of a broken declaration list (PLAN.md W5.1).
/// The module parse stops at the first unreadable declaration, and the grammar
/// has no recovery production there. The `ConstDeclParser`, `TypeDeclParser`,
/// and `VarDeclParser` entry points each read one declaration. This function
/// splits the token stream at depth-zero semicolons, then asks the matching
/// parser to check each chunk. Valid chunks leave diagnostics unchanged, so a
/// missing `;` produces one message rather than one per declaration.
fn declaration_diagnostics(input: &str, dialect: Dialect) -> Vec<Diagnostic> {
    let mut toks: Vec<(usize, Token<'_>, usize)> = Vec::new();
    for next in dialect.lexer(input) {
        match next {
            Ok(t) => toks.push(t),
            // A lexical error is the caller's to report; there is no point
            // guessing at declarations after it.
            Err(_) => return Vec::new(),
        }
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let kind = match toks[i].1 {
            Token::Const => 0,
            Token::Type => 1,
            Token::Var => 2,
            _ => {
                i += 1;
                continue;
            }
        };
        i += 1;
        // Scan one section's declarations, separated by `;`, until the next
        // section keyword, module `BEGIN`, or `PROCEDURE`. Semicolons inside a
        // `RECORD` or parentheses do not separate declarations, so track depth.
        loop {
            let mut depth = 0i32;
            let mut j = i;
            let mut ended_by_semi = false;
            while j < toks.len() {
                match &toks[j].1 {
                    Token::Record | Token::LParen | Token::LBrack => depth += 1,
                    Token::RParen | Token::RBrack => depth -= 1,
                    Token::End if depth > 0 => depth -= 1,
                    Token::Semi if depth == 0 => {
                        ended_by_semi = true;
                        break;
                    }
                    Token::Const
                    | Token::Type
                    | Token::Var
                    | Token::Begin
                    | Token::Procedure
                    | Token::End
                        if depth == 0 =>
                    {
                        break;
                    }
                    _ => {}
                }
                j += 1;
            }
            if j > i {
                let chunk = &toks[i..j];
                let mut sink = Vec::new();
                // These parsers return different types, so only check for errors.
                let failed = match kind {
                    0 => crate::oberon::ConstDeclParser::new()
                        .parse(&mut sink, chunk.iter().cloned())
                        .err(),
                    1 => crate::oberon::TypeDeclParser::new()
                        .parse(&mut sink, chunk.iter().cloned())
                        .err(),
                    _ => crate::oberon::VarDeclParser::new()
                        .parse(&mut sink, chunk.iter().cloned())
                        .err(),
                };
                if let Some(e) = failed {
                    let mut d = diagnostic_for_parse_error(&e);
                    // A chunk is not the whole file, so "unexpected end of file"
                    // would be a lie: what ran out is the declaration.
                    if matches!(e, lalrpop_util::ParseError::UnrecognizedEof { .. }) {
                        d.message = "this declaration is incomplete: the parser ran out of it \
                                    before the `;`"
                            .to_string();
                    }
                    out.push(d);
                }
                let _ = sink;
            }
            if !ended_by_semi {
                // The section ended without a `;`: the next token is either the
                // next section's keyword or the body, and the outer loop takes it.
                i = j;
                break;
            }
            i = j + 1;
            if i >= toks.len() {
                break;
            }
        }
    }
    out
}
