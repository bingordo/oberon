//! Oberon-07 front end prototype: lexer + LALRPOP grammar + AST.
//!
//! This crate exists to *verify* the design in ../DESIGN.md: that the
//! report's grammar, with the two factoring decisions described in
//! `oberon.lalrpop`, is accepted by LALRPOP's LR(1) (lane table) construction
//! without conflicts, and that realistic Oberon-07 modules parse.

pub mod ast;
pub mod cgen;
pub mod const_eval;
pub mod lexer;
pub mod resolve;
pub mod sema;
pub mod sym;
pub mod types;

// LALRPOP's generated parser takes `&mut Vec<ErrorRecovery<..>>` for the
// recovery hook, which is its API, not a choice of ours: `clippy::ptr_arg`
// fires 244 times inside the generated file and nowhere in ours (PLAN.md W0.1).
lalrpop_util::lalrpop_mod!(#[allow(clippy::ptr_arg)] pub oberon);

use ast::Module;
use lexer::{Lexer, LexicalError, Token};
use lalrpop_util::{ErrorRecovery, ParseError};

pub type ParseErr<'input> = ParseError<usize, Token<'input>, LexicalError>;

/// Parse a module, failing at the first syntax error.
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

pub use sema::Diagnostic;

use std::fmt::Write as _;

/// Check a module and, if it is clean, emit its C translation unit and header.
/// Returns `Err` with the diagnostics when the module does not check: the
/// emitter assumes the checks passed and would otherwise emit nonsense.
/// The interface file text for a module, or `None` when it does not check.
pub fn emit_interface(input: &str, interfaces: &[sym::Interface]) -> Option<String> {
    emit_interface_dialect(input, interfaces, Dialect::Report)
}

pub fn emit_interface_dialect(
    input: &str,
    interfaces: &[sym::Interface],
    dialect: Dialect,
) -> Option<String> {
    let mut m = parse_module_dialect(input, dialect).ok()?;
    let sema = sema::Sema::check_owned(&mut m, interfaces);
    if sema.diagnostics().iter().any(Diagnostic::is_error) {
        return None;
    }
    Some(sym::render_dialect(&cgen::interface_of(&m, &sema.types, &sema), dialect))
}

pub fn emit_c<'input>(
    input: &'input str,
    interfaces: &[sym::Interface],
) -> Result<Result<(String, String), Vec<Diagnostic>>, ParseErr<'input>> {
    emit_c_with_checks(input, interfaces, cgen::Checks::ALL)
}

/// The same, with the runtime checks under the caller's control: `-Ochecks=n`
/// removes the index, `NIL` and guard checks and the type-`CASE` fallback
/// (DESIGN §9.4, PLAN.md W3.1).
pub fn emit_c_with_checks<'input>(
    input: &'input str,
    interfaces: &[sym::Interface],
    checks: cgen::Checks,
) -> Result<Result<(String, String), Vec<Diagnostic>>, ParseErr<'input>> {
    emit_c_full(input, interfaces, checks, Dialect::Report)
}

/// The same, in a chosen dialect (PLAN.md W8.10).
pub fn emit_c_full<'input>(
    input: &'input str,
    interfaces: &[sym::Interface],
    checks: cgen::Checks,
    dialect: Dialect,
) -> Result<Result<(String, String), Vec<Diagnostic>>, ParseErr<'input>> {
    let mut m = parse_module_dialect(input, dialect)?;
    let sema = sema::Sema::check_owned(&mut m, interfaces);
    if sema.diagnostics().iter().any(Diagnostic::is_error) {
        return Ok(Err(sema.diags.clone()));
    }
    let (c, h) = cgen::emit_module_checked(&m, &sema.types, &sema, checks);
    Ok(Ok((c, h)))
}

/// Byte offset → `line:column` (1-based), the only place line information is
/// computed (DESIGN §10): the AST carries byte spans, and whoever prints a
/// diagnostic pays for the conversion.
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

fn severity_name(severity: sema::Severity) -> &'static str {
    match severity {
        sema::Severity::Error => "error",
        sema::Severity::Warning => "warning",
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

/// A syntax error as a first-class diagnostic, so that a parse failure is
/// located and rendered exactly like a type error (DESIGN §5 D7, §10).
/// The note for a recovered error: what the parser threw away to carry on.
/// Every dropped token is a token of the *input*, so nothing is filtered: a
/// program may legally declare `error` or `Eof` (report §3), and dropping those
/// from the note would misdescribe what was skipped.
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

pub fn diagnostic_for_parse_error(error: &ParseErr<'_>) -> Diagnostic {
    use lalrpop_util::ParseError as P;
    let expected_note = |expected: &[String]| -> Option<String> {
        if expected.is_empty() {
            return None;
        }
        // LALRPOP names terminals with their own quoting; a reader wants the
        // tokens themselves.
        let clean = |s: &String| s.trim_matches('"').to_string();
        // A missing separator is the commonest defect, so `;` leads when it
        // would have fitted; the rest keeps LALRPOP's order.
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

fn span_of(lo: usize, hi: usize) -> ast::Span {
    ast::Span::new(lo, hi)
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

/// Parse a module and turn every syntax error into a located diagnostic —
/// including the fatal one, which a recovery point could not absorb (a missing
/// `END`, or a lexical error). The module comes back when the parser got far
/// enough to build one.
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
                // A reserved word the program spelled in lower case is only one
                // because this dialect folds case; say so (W8.10).
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
    match oberon::CompilationUnitParser::new().parse(&mut errors, dialect.lexer(input)) {
        Ok(module) => (Some(module), recovered(&errors)),
        Err(fatal) => {
            let mut diags = recovered(&errors);
            diags.push(diagnostic_for_parse_error(&fatal));
            (None, diags)
        }
    }
}

/// Parse *and* check a module, reporting syntax errors and semantic errors in
/// one run: the grammar recovers at statement level, so a file with three bad
/// statements is still type-checked.
pub fn check_module_recovering(
    input: &str,
    interfaces: &[sym::Interface],
) -> (Option<Module>, Vec<Diagnostic>) {
    check_module_recovering_dialect(input, interfaces, Dialect::Report)
}

pub fn check_module_recovering_dialect(
    input: &str,
    interfaces: &[sym::Interface],
    dialect: Dialect,
) -> (Option<Module>, Vec<Diagnostic>) {
    let (module, mut diags) = parse_with_diagnostics_dialect(input, dialect);
    match module {
        Some(mut m) => {
            let (_, mut sem) = sema::Sema::check(&mut m, interfaces);
            diags.append(&mut sem);
            sema::dedup_diagnostics(&mut diags);
            diags.sort_by_key(|d| (d.span.lo, d.span.hi));
            (Some(m), diags)
        }
        None => (None, diags),
    }
}

/// Parse a module and run the semantic pass over it (DESIGN §7-§8). The module
/// comes back with its selectors reclassified, so tests can inspect both.
pub fn check_module<'input>(
    input: &'input str,
    interfaces: &[sym::Interface],
) -> Result<(Module, Vec<Diagnostic>), ParseErr<'input>> {
    let mut m = parse_module(input)?;
    let (_, diags) = sema::Sema::check(&mut m, interfaces);
    Ok((m, diags))
}

/// The run of `Out` from the report's chapter 11, verbatim.
#[allow(dead_code)]
const OUT_MODULE: &str = r#"
MODULE Out; (* exported procedures: Write, WriteInt, WriteLn *)
  IMPORT Texts, Oberon;
  VAR W: Texts.Writer;

  PROCEDURE Write*(ch: CHAR);
  BEGIN Texts.Write(W, ch) END Write;

  PROCEDURE WriteInt*(x, n: INTEGER);
VAR i: INTEGER; a: ARRAY 16 OF CHAR;
  BEGIN
i := 0;
IF x < 0 THEN Texts.Write(W, "-"); x := -x END;
REPEAT
  a[i] := CHR(x MOD 10 + ORD("0")); x := x DIV 10; INC(i)
UNTIL x = 0;
REPEAT Texts.Write(W, " "); DEC(n) UNTIL n <= i;
REPEAT DEC(i); Texts.Write(W, a[i]) UNTIL i = 0
  END WriteInt;

  PROCEDURE WriteLn*;
  BEGIN Texts.WriteLn(W); Texts.Append(Oberon.Log, W.buf) END WriteLn;

BEGIN Texts.OpenWriter(W) END Out.
"#;

/// Declarations, types, control flow, sets, type tests and type guards --
/// every construct the report describes, in one module.
#[allow(dead_code)]
const KITCHEN_SINK: &str = r#"
MODULE Demo;
  IMPORT M := Other, Strings;
  CONST N = 100; limit = 2*N - 1; all = {0 .. 8*4 - 1}; name = "Oberon";
  TYPE
Table = ARRAY N OF REAL;
Matrix = ARRAY 10, 20 OF INTEGER;
Tree = POINTER TO Node;
Node = RECORD key: INTEGER; left, right: Tree END;
CenterNode = RECORD (Node) name: ARRAY 32 OF CHAR; subnode: Tree END;
Function* = PROCEDURE (x: INTEGER; VAR y: REAL): INTEGER;
  VAR
i, j, k: INTEGER; x, y: REAL; p, q: BOOLEAN; s: SET; a: Table;
w: ARRAY 16 OF RECORD ch: CHAR; count: INTEGER END;
t: Tree; f: Function;

  PROCEDURE log2(x: INTEGER): INTEGER;
VAR y: INTEGER; (* assume x > 0 *)
  BEGIN y := 0; WHILE x > 1 DO x := x DIV 2; INC(y) END; RETURN y
  END log2;

  PROCEDURE Walk*(VAR t: Tree; n: INTEGER; OPEN: ARRAY OF CHAR);
CONST local = 3;
VAR c: CenterNode; r: REAL;
  BEGIN
IF (t # NIL) & (t.key # n) THEN
  CASE t.key OF
    0: t := t.left
  | 1..3: t := t.right
  | 4, 5: t := NIL
  END
ELSIF t IS CenterNode THEN
  c := t(CenterNode);
  c.name := "A"
ELSE
  k := 1
END;
CASE t OF
  Tree: k := 2
| : k := 3
END;
FOR i := 0 TO limit BY local DO s := s + {i} END;
FOR i := limit TO 0 BY -1 DO s := s - {i .. i+1} END;
WHILE (i > 0) & (j < 10) DO i := i - 1 ELSIF x = 0.0 DO j := j + 1 END;
REPEAT i := i + 1 UNTIL (i >= j) OR p;
s := (s + all) * all - all / all;
x := FLT(i) + ABS(y);
IF 3 IN s THEN p := ~p END;
IF (0 <= i) & (i < 100) THEN k := (i + j) DIV 2 END;
r := 4.567E8 * 12.3;
j := -k; j := ORD("A"); j := ORD(22X)
  END Walk;
BEGIN
i := log2(8); i := 0; s := {2, 3, 5, 7, 11, 13}; Walk(t, i, "hi")
END Demo.
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::*;
    use crate::lexer::tokenize;


    #[test]
    fn lexer_near_misses() {
        // 22X is a character constant, not "22" then "X".
        assert!(matches!(tokenize("22X")[0], Ok((0, Token::CharLit(0x22), 3))));
        // 100H / 0FFH are hex integers.
        assert!(matches!(tokenize("100H")[0], Ok((0, Token::Integer(0x100), 4))));
        assert!(matches!(tokenize("0FFH")[0], Ok((0, Token::Integer(0xff), 4))));
        // 1..5 must be integer, range, integer.
        let t = tokenize("1..5");
        assert!(matches!(t[0], Ok((0, Token::Integer(1), 1))));
        assert!(matches!(t[1], Ok((1, Token::DotDot, 3))));
        assert!(matches!(t[2], Ok((3, Token::Integer(5), 4))));
        // 1.5E3 is a real.
        assert!(matches!(tokenize("1.5E3")[0], Ok((0, Token::Real(v), 5)) if v == 1500.0));
        // comments nest.
        assert_eq!(tokenize("(* a (* b *) c *) 7").len(), 1);
        // keywords are exact-case reserved words only.
        assert!(matches!(tokenize("If")[0], Ok((_, Token::Ident("If"), _))));
        assert!(matches!(tokenize("IF")[0], Ok((_, Token::If, _))));
    }

    #[test]
    fn parses_report_out_module() {
        let m = parse_module(OUT_MODULE).expect("Out module must parse");
        assert_eq!(m.name, "Out");
        assert_eq!(m.end_name, "Out");
        assert_eq!(m.imports.len(), 2);
        assert_eq!(m.imports[0].alias, "Texts");
        assert_eq!(m.decls.vars.len(), 1);
        assert_eq!(m.decls.procs.len(), 3);
        let write_int = &m.decls.procs[1];
        assert_eq!(write_int.name, "WriteInt");
        assert!(write_int.export);
        assert_eq!(write_int.end_name, "WriteInt");
        // One FPSection holding both names, as the report writes it.
        assert_eq!(write_int.params.len(), 1);
        assert_eq!(write_int.params[0].names.len(), 2);
        assert_eq!(write_int.params[0].ty.base, vec!["INTEGER"]);
        assert_eq!(write_int.decls.vars.len(), 2);
        assert_eq!(write_int.body.len(), 5);
    }


    #[test]
    fn parses_kitchen_sink() {
        let m = parse_module(KITCHEN_SINK).expect("kitchen sink must parse");
        assert_eq!(m.name, "Demo");
        assert_eq!(m.imports.len(), 2);
        assert_eq!(m.imports[0].alias, "M");
        assert_eq!(m.imports[0].module, "Other");
        assert_eq!(m.decls.consts.len(), 4);
        assert_eq!(m.decls.types.len(), 6);
        assert_eq!(m.decls.vars.len(), 8);
        assert_eq!(m.decls.procs.len(), 2);

        // Matrix is the report's abbreviation ARRAY 10 OF ARRAY 20 OF INTEGER
        // spelled as two lengths; the AST keeps both dimensions.
        match &m.decls.types[1].ty {
            TypeExpr::Array { dims, elem, .. } => {
                assert_eq!(dims.len(), 2);
                match &**elem {
                    TypeExpr::Named(q, _) => assert_eq!(q, &vec!["INTEGER".to_string()]),
                    other => panic!("expected named element type, got {other:?}"),
                }
            }
            other => panic!("expected array, got {other:?}"),
        }
        // CenterNode extends Node.
        match &m.decls.types[4].ty {
            TypeExpr::Record { base, fields, .. } => {
                assert_eq!(base.as_ref().unwrap(), &vec!["Node".to_string()]);
                assert_eq!(fields.len(), 2);
            }
            other => panic!("expected record, got {other:?}"),
        }

        let walk = &m.decls.procs[1];
        assert_eq!(walk.name, "Walk");
        assert_eq!(walk.params.len(), 3);
        assert!(walk.params[0].by_ref, "VAR t: Tree");
        // `OPEN: ARRAY OF CHAR` is one open array of CHAR.
        assert_eq!(walk.params[2].ty.open, 1);
        assert_eq!(walk.params[2].ty.base, vec!["CHAR"]);
        // `RETURN` is a statement now (W8.4), so what this pins is that the body
        // has none — the report's tail clause was optional too.
        assert!(walk
            .body
            .iter()
            .all(|s| !matches!(s.kind, StmtKind::Return(_))));

        // The `t(CenterNode)` type guard parses as a designator selector whose
        // single "argument" is a bare designator naming a type; the semantic
        // phase is what turns it into a guard.
        let if_stmt = &walk.body[0];
        match &if_stmt.kind {
            StmtKind::If { arms, els } => {
                assert_eq!(arms.len(), 2, "IF arm plus one ELSIF arm");
                assert!(els.is_some());
                let assign = &arms[1].1[0];
                match &assign.kind {
                    StmtKind::Assign { value, .. } => match &value.kind {
                        ExprKind::Designator(d) => {
                            assert_eq!(d.base, vec!["t".to_string()]);
                            match &d.sels[..] {
                                [Selector::Args(Some(args))] => match &args[0].kind {
                                    ExprKind::Designator(inner) => {
                                        assert_eq!(inner.base, vec!["CenterNode".to_string()]);
                                        assert!(inner.sels.is_empty());
                                    }
                                    other => panic!("guard argument: {other:?}"),
                                },
                                other => panic!("expected one Args selector, got {other:?}"),
                            }
                        }
                        other => panic!("expected designator value, got {other:?}"),
                    },
                    other => panic!("expected assignment, got {other:?}"),
                }
            }
            other => panic!("expected IF, got {other:?}"),
        }

        // The record CASE has type labels, the integer CASE has ranges.
        match &walk.body[1].kind {
            StmtKind::Case { arms, .. } => {
                assert_eq!(arms.len(), 2);
                match &arms[0].labels.as_ref().unwrap()[0].lo {
                    CaseLabel::Named(q) => assert_eq!(q, &vec!["Tree".to_string()]),
                    other => panic!("expected type label, got {other:?}"),
                }
            }
            other => panic!("expected CASE, got {other:?}"),
        }
        match &walk.body[2].kind {
            StmtKind::For { var, by, .. } => {
                assert_eq!(var, "i");
                assert!(by.is_some(), "BY clause");
            }
            other => panic!("expected FOR, got {other:?}"),
        }
    }

    #[test]
    fn type_guard_and_call_share_one_production() {
        let src = "MODULE M; VAR t: T; x: INTEGER;
                   PROCEDURE f(a: INTEGER): INTEGER; BEGIN RETURN a END f;
                   BEGIN t := t(T); x := f(1) END M.";
        let m = parse_module(src).expect("guard and call must parse");
        // `t(T)`: selector Args with one bare designator, no further selectors.
        let assign_stmt = &m.body[0];
        match &assign_stmt.kind {
            StmtKind::Assign { value, .. } => match &value.kind {
                ExprKind::Designator(d) => {
                    assert_eq!(d.base, vec!["t".to_string()]);
                    assert!(matches!(d.sels[0], Selector::Args(Some(_))));
                }
                other => panic!("expected designator, got {other:?}"),
            },
            other => panic!("expected assignment, got {other:?}"),
        }
        // `f(1)`: same shape, so the grammar needs no separate call rule.
        match &m.body[1].kind {
            StmtKind::Assign { value, .. } => match &value.kind {
                ExprKind::Designator(d) => {
                    assert_eq!(d.base, vec!["f".to_string()]);
                    assert!(matches!(d.sels[0], Selector::Args(Some(_))));
                }
                other => panic!("expected designator, got {other:?}"),
            },
            other => panic!("expected assignment, got {other:?}"),
        }
    }

    #[test]
    fn reports_syntax_errors_without_panicking() {
        // Missing END.
        let e = parse_module("MODULE M; VAR x: INTEGER; BEGIN x := 1 M.").unwrap_err();
        assert!(matches!(e, ParseError::UnrecognizedToken { .. }));
        // Unterminated comment is a lexer error.
        let e = parse_module("MODULE M; (* oops ").unwrap_err();
        assert!(matches!(e, ParseError::User { .. }));
    }

    #[test]
    fn type_guard_can_be_followed_by_selectors() {
        // The report's `t(CenterNode).subnode (Tree)`: a guard sits in the
        // middle of a designator, which is why `( ... )` has to be a selector
        // rather than a trailing "actual parameters" production.
        let src = "MODULE M; VAR t: Tree; r: Node;
                   BEGIN r := t(CenterNode).subnode^.left END M.";
        let m = parse_module(src).expect("guarded designator must parse");
        match &m.body[0].kind {
            StmtKind::Assign { value, .. } => match &value.kind {
                ExprKind::Designator(d) => {
                    assert_eq!(d.base, vec!["t".to_string()]);
                    assert!(matches!(d.sels[0], Selector::Args(Some(_))));
                    assert!(matches!(d.sels[1], Selector::Field(_)));
                    assert!(matches!(d.sels[2], Selector::Deref));
                    assert_eq!(d.sels.len(), 4);
                }
                other => panic!("expected designator, got {other:?}"),
            },
            other => panic!("expected assignment, got {other:?}"),
        }
    }

    #[test]
    fn empty_statements_are_legal() {
        let src = "MODULE M; VAR x: INTEGER; BEGIN ; x := 1; ; END M.";
        let m = parse_module(src).expect("empty statements must parse");
        // `; x := 1; ; ` -> the empty slots disappear from the AST, the one
        // real statement remains.
        assert_eq!(m.body.len(), 1);
    }

    /// A module that exercises every branch of the reclassification pass.
    const RECLASSIFY: &str = r#"
MODULE M;
  IMPORT Out := Texts, SYSTEM;
  TYPE T = POINTER TO R; R = RECORD f: INTEGER END;
  VAR t: T; x: INTEGER;
  PROCEDURE g(a: INTEGER): INTEGER; BEGIN RETURN a END g;
BEGIN
  t := t(T);
  x := g(1);
  x := t(T).f;
  Out.Write(x);
  x := t.f;
  INC(x);
  x := SYSTEM.SIZE(T)
END M.
"#;

    #[test]
    fn reclassify_splits_guards_calls_and_module_qualification() {
        use crate::ast::Selector as Sel;
        use crate::resolve::{reclassify, Names};

        let mut m = parse_module(RECLASSIFY).expect("module must parse");
        let mut names = Names::collect(&m);
        reclassify(&mut m, &mut names).expect("reclassification must succeed");

        let value_of = |i: usize| match &m.body[i].kind {
            StmtKind::Assign { value, .. } => value.clone(),
            other => panic!("statement {i} is not an assignment: {other:?}"),
        };
        let designator_of = |e: &Expr| match &e.kind {
            ExprKind::Designator(d) => d.clone(),
            other => panic!("expected a designator, got {other:?}"),
        };

        // 0: `t := t(T)` - a single argument that names a type is a guard.
        let d = designator_of(&value_of(0));
        assert_eq!(d.base, vec!["t".to_string()]);
        assert_eq!(d.sels, vec![Sel::Guard(vec!["T".to_string()])]);

        // 1: `x := g(1)` - a single argument that is not a type is a call.
        let d = designator_of(&value_of(1));
        assert_eq!(d.base, vec!["g".to_string()]);
        match &d.sels[..] {
            [Sel::Call(args)] => assert_eq!(args.len(), 1),
            other => panic!("expected one call, got {other:?}"),
        }

        // 2: `x := t(T).f` - guard, then a field select.
        let d = designator_of(&value_of(2));
        assert_eq!(
            d.sels,
            vec![
                Sel::Guard(vec!["T".to_string()]),
                Sel::Field("f".to_string())
            ]
        );

        // 3: `Out.Write(x)` - the import alias folds into the base.
        match &m.body[3].kind {
            StmtKind::Call(d) => {
                assert_eq!(d.base, vec!["Out".to_string(), "Write".to_string()]);
                match &d.sels[..] {
                    [Sel::Call(args)] => assert_eq!(args.len(), 1),
                    other => panic!("expected one call, got {other:?}"),
                }
            }
            other => panic!("statement 3 is not a call: {other:?}"),
        }

        // 4: `x := t.f` - `t` is not a module, so this stays a field select.
        let d = designator_of(&value_of(4));
        assert_eq!(d.base, vec!["t".to_string()]);
        assert_eq!(d.sels, vec![Sel::Field("f".to_string())]);

        // 5: `INC(x)` - a predeclared procedure is an ordinary call.
        match &m.body[5].kind {
            StmtKind::Call(d) => {
                assert_eq!(d.base, vec!["INC".to_string()]);
                match &d.sels[..] {
                    [Sel::Call(args)] => assert_eq!(args.len(), 1),
                    other => panic!("expected one call, got {other:?}"),
                }
            }
            other => panic!("statement 5 is not a call: {other:?}"),
        }

        // 6: `SYSTEM.SIZE(T)` - report §11.1's `SIZE` takes a *type*, so rule
        // (b) would read `(T)` as a guard; the entry's name is what keeps it a
        // call (DESIGN §9.10).
        let d = designator_of(&value_of(6));
        assert_eq!(d.base, vec!["SYSTEM".to_string(), "SIZE".to_string()]);
        match &d.sels[..] {
            [Sel::Call(args)] => assert_eq!(args.len(), 1),
            other => panic!("expected one call, got {other:?}"),
        }
    }

    #[test]
    fn reclassify_leaves_no_unresolved_selector_in_the_kitchen_sink() {
        use crate::ast::Selector as Sel;
        use crate::resolve::{reclassify, Names};

        let mut m = parse_module(KITCHEN_SINK).expect("kitchen sink must parse");
        let mut names = Names::collect(&m);
        reclassify(&mut m, &mut names).expect("reclassification must succeed");
        // `CASE t OF Tree: ...` and `t(CenterNode)` inside the sink.
        let (mut guards, mut calls, mut args) = (0, 0, 0);

        fn walk_desig(d: &Designator, g: &mut i32, c: &mut i32, a: &mut i32) {
            for s in &d.sels {
                match s {
                    Sel::Guard(_) => *g += 1,
                    Sel::Call(es) => {
                        *c += 1;
                        for e in es {
                            walk_expr(e, g, c, a);
                        }
                    }
                    Sel::Args(_) => *a += 1,
                    Sel::Index(es) => {
                        for e in es {
                            walk_expr(e, g, c, a);
                        }
                    }
                    Sel::Field(_) | Sel::Deref => {}
                }
            }
        }
        fn walk_expr(e: &Expr, g: &mut i32, c: &mut i32, a: &mut i32) {
            match &e.kind {
                ExprKind::Designator(d) => walk_desig(d, g, c, a),
                ExprKind::Unary(_, x) => walk_expr(x, g, c, a),
                ExprKind::Binary(_, l, r) => {
                    walk_expr(l, g, c, a);
                    walk_expr(r, g, c, a);
                }
                ExprKind::TypeTest(x, _) => walk_expr(x, g, c, a),
                ExprKind::Set(elems) => {
                    for el in elems {
                        walk_expr(&el.lo, g, c, a);
                        if let Some(h) = &el.hi {
                            walk_expr(h, g, c, a);
                        }
                    }
                }
                ExprKind::Int(_)
                | ExprKind::Real(_)
                | ExprKind::Char(_)
                | ExprKind::Str(_)
                | ExprKind::Bool(_)
                | ExprKind::Nil => {}
            }
        }
        fn walk_stmts(ss: &[Stmt], g: &mut i32, c: &mut i32, a: &mut i32) {
            for s in ss {
                match &s.kind {
                    StmtKind::Assign { target, value } => {
                        walk_desig(target, g, c, a);
                        walk_expr(value, g, c, a);
                    }
                    StmtKind::Call(d) => walk_desig(d, g, c, a),
                    StmtKind::Exit => {}
                    StmtKind::Return(e) => {
                        if let Some(e) = e {
                            walk_expr(e, g, c, a);
                        }
                    }
                    StmtKind::If { arms, els } => {
                        for (cond, b) in arms {
                            walk_expr(cond, g, c, a);
                            walk_stmts(b, g, c, a);
                        }
                        if let Some(b) = els {
                            walk_stmts(b, g, c, a);
                        }
                    }
                    StmtKind::Case { subject, arms } => {
                        walk_expr(subject, g, c, a);
                        for arm in arms {
                            walk_stmts(&arm.body, g, c, a);
                        }
                    }
                    StmtKind::While { arms } => {
                        for (cond, b) in arms {
                            walk_expr(cond, g, c, a);
                            walk_stmts(b, g, c, a);
                        }
                    }
                    StmtKind::Repeat { body, until } => {
                        walk_stmts(body, g, c, a);
                        walk_expr(until, g, c, a);
                    }
                    StmtKind::For { from, to, by, body, .. } => {
                        walk_expr(from, g, c, a);
                        walk_expr(to, g, c, a);
                        if let Some(b) = by {
                            walk_expr(b, g, c, a);
                        }
                        walk_stmts(body, g, c, a);
                    }
                }
            }
        }
        let mut stmts = m.body.clone();
        for p in &m.decls.procs {
            stmts.extend(p.body.clone());
        }
        walk_stmts(&stmts, &mut guards, &mut calls, &mut args);
        assert_eq!(args, 0, "every `( ... )` selector must be resolved");
        assert_eq!(guards, 1, "`t(CenterNode)` is the only guard in the sink");
        assert_eq!(calls, 7, "INC, FLT, ABS, ORD, ORD, Walk, log2");
    }

    #[test]
    fn reclassify_rejects_a_case_label_that_is_neither_type_nor_module() {
        use crate::resolve::{reclassify, Names};

        let src = "MODULE M; VAR t: T; BEGIN CASE t OF NotAType: END END M.";
        let mut m = parse_module(src).expect("module must parse");
        let mut names = Names::collect(&m);
        let e = reclassify(&mut m, &mut names).expect_err("must be rejected");
        assert!(e.message.contains("NotAType"), "{}", e.message);

        // The same shape with a declared type is accepted.
        let src = "MODULE M; TYPE T = POINTER TO R; R = RECORD f: INTEGER END;
                   VAR t: T; BEGIN CASE t OF T: END END M.";
        let mut m = parse_module(src).expect("module must parse");
        let mut names = Names::collect(&m);
        reclassify(&mut m, &mut names).expect("`T` is a declared type");
    }
}

    #[test]
    fn an_inherited_private_field_and_a_qualified_label_behave() {
        // Three defects found by review of the visibility rule, all in one place.
        let iface = sym::parse(
            "MODULE M\nVERSION 1\nTYPE Base = RECORD secret: INTEGER END\nTYPE Node = RECORD child*: RECORD hidden: INTEGER END; next*: POINTER TO Node END\nTYPE DerivedPtr = POINTER TO Derived\nTYPE Derived = RECORD (Base) extra*: INTEGER END\n",
        )
        .unwrap();

        // (a) an inherited private field cannot be reached through a *local*
        //     derived record: the mark that counts is the declaring record's.
        let src = "MODULE C; IMPORT M; TYPE D = RECORD (M.Base) END; VAR d: D;\nBEGIN d.secret := 1 END C.\n";
        let (_, diags) = check_module_recovering(src, std::slice::from_ref(&iface));
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("`secret` is private to module `M`")),
            "{:#?}",
            render_diagnostics("C.mod", src, &diags)
        );

        // (b) a record nested in a recursive imported type is still marked: the
        //     walk used to spin on the pointer and leave `child` unvisited.
        let src = "MODULE C; IMPORT M; VAR n: M.Node;\nBEGIN n.child.hidden := 1 END C.\n";
        let (_, diags) = check_module_recovering(src, std::slice::from_ref(&iface));
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("`hidden` is private to module `M`")),
            "{:#?}",
            render_diagnostics("C.mod", src, &diags)
        );

        // (c) a *qualified* type-CASE label still narrows the arm.
        let src = "MODULE C; IMPORT M; VAR p: POINTER TO M.Base;\nBEGIN CASE p OF M.DerivedPtr: p.extra := 1 END END C.\n";
        let (_, diags) = check_module_recovering(src, std::slice::from_ref(&iface));
        assert!(
            diags.iter().all(|d| !d.is_error()),
            "{}",
            render_diagnostics("C.mod", src, &diags)
        );
    }

    #[test]
    fn a_private_field_of_an_imported_record_cannot_be_named() {
        // ✔ rows 4 and 14, the private-field halves: a field declared without the
        // report's `*` is private to its module (report §6.3), and the mark
        // travels through the interface.
        let iface = sym::parse(
            "MODULE Texts\nVERSION 1\nTYPE Buffer = ARRAY 64 OF CHAR\nTYPE Writer = RECORD buf: Buffer; pos*: INTEGER END\n",
        )
        .unwrap();

        let src = "MODULE M; IMPORT Texts; VAR w: Texts.Writer;\nBEGIN w.buf[0] := 0X END M.\n";
        let (_, diags) = check_module_recovering(src, std::slice::from_ref(&iface));
        let text = render_diagnostics("M.mod", src, &diags);
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("`buf` is private to module `Texts`")),
            "{text}"
        );

        // ... while a marked field is exactly what the mark is for.
        let src = "MODULE M; IMPORT Texts; VAR w: Texts.Writer;\nBEGIN w.pos := 1 END M.\n";
        let (_, diags) = check_module_recovering(src, std::slice::from_ref(&iface));
        assert!(
            diags.iter().all(|d| !d.is_error()),
            "{}",
            render_diagnostics("M.mod", src, &diags)
        );

        // ... and a module's own unmarked field is nobody else's business.
        let src = "MODULE M; TYPE R = RECORD f: INTEGER END; VAR r: R;\nBEGIN r.f := 1 END M.\n";
        let (_, diags) = check_module_recovering(src, &[]);
        assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
    }

#[cfg(test)]
mod emit_tests {
    use super::*;

    /// Emit a module's C, or panic with the diagnostics.
    fn c_of(src: &str) -> String {
        match emit_c(src, &[]) {
            Ok(Ok((c, _h))) => c,
            Ok(Err(diags)) => panic!("module did not check: {diags:#?}"),
            Err(e) => panic!("syntax error: {e}"),
        }
    }

    #[test]
    fn a_pointer_access_is_checked_for_nil() {
        // R21: `p.f` and `p^` used to be emitted as a plain dereference, so a NIL
        // pointer faulted instead of trapping with a location. The check is part
        // of the access, which is why an assignment to a field of a pointer
        // target still compiles.
        let src = "MODULE M;\n  TYPE R = RECORD x: INTEGER END;\n  VAR p: POINTER TO R;\nBEGIN\n  p.x := 1;\n  p^ := p^\nEND M.\n";
        let c = c_of(src);
        assert!(c.contains("rt_nil("), "{c}");
        assert!(c.contains("rt_nil(") && c.contains(")->x"), "{c}");
        // ... but not when the operand can have a side effect: the check reads it
        // twice, so an index expression containing a call would be evaluated
        // twice and could check one pointer while dereferencing another. The
        // check is left out there (the fault remains) rather than emitted wrong.
        let src = "MODULE M;\n  TYPE R = RECORD x: INTEGER END;\n  VAR a: ARRAY 2 OF POINTER TO R;\n\
  PROCEDURE Next(): INTEGER; BEGIN RETURN 0 END Next;\n\
BEGIN\n  a[Next()].x := 1\nEND M.\n";
        let c = c_of(src);
        assert!(!c.contains("rt_nil("), "{c}");

        // a plain pointer assignment and a NIL test are not dereferences
        let src = "MODULE M;\n  TYPE R = RECORD x: INTEGER END;\n  VAR p, q: POINTER TO R;\nBEGIN\n  p := q;\n  IF p # NIL THEN END\nEND M.\n";
        let c = c_of(src);
        assert!(!c.contains("rt_nil("), "{c}");
    }

    #[test]
    fn a_shadowed_builtin_call_is_not_folded_away() {
        // `ABS` is a predefined name, but a nested procedure or a parameter may
        // legally shadow it. Folding by *spelling* would remove the call and its
        // side effect: the emitted C must contain the call.
        // A module-level declaration shadows the predefined name (report §4:
        // the universe of predefined identifiers is the outermost scope), and so
        // does a nested one.
        let src = "MODULE M;\n  VAR m0: INTEGER;\n\
  PROCEDURE ABS(x: INTEGER): INTEGER;\n  BEGIN m0 := m0 + 1; RETURN 42 END ABS;\n\
BEGIN\n  m0 := 0; m0 := ABS(1)\nEND M.\n";
        let c = c_of(src);
        assert!(c.contains("M__ABS("), "{c}");
        assert!(!c.contains("m0 = ((int32_t)42u)"), "{c}");
    }

    #[test]
    fn a_folded_char_keeps_its_c_type() {
        // `Const::int()` also answers for a CHAR; taking that path would emit an
        // `int32_t` where a `char` belongs, which `-Werror` rejects on assignment
        // where plain `char` is signed.
        let src = "MODULE M; VAR ch: CHAR; BEGIN ch := 0FFX END M.\n";
        let c = c_of(src);
        assert!(c.contains("((char)255u)"), "{c}");
        assert!(!c.contains("((int32_t)255u)"), "{c}");
    }

    #[test]
    fn a_self_comparison_is_folded_not_emitted() {
        // `x = x` is a tautology the C compiler warns about, and the build line
        // treats warnings as errors. Parsed operands carry different spans, so a
        // span-sensitive comparison would miss it.
        let src = "MODULE M; VAR x: INTEGER; BEGIN IF x = x THEN x := 1 END END M.\n";
        let c = c_of(src);
        assert!(c.contains("if (1)"), "{c}");
        assert!(!c.contains("=="), "{c}");

        // ... but a REAL is not folded: `x = x` is false for NaN.
        let src = "MODULE M; VAR r: REAL; BEGIN IF r = r THEN r := 1.0 END END M.\n";
        let c = c_of(src);
        assert!(c.contains("=="), "{c}");
    }
}

#[cfg(test)]
mod diag_tests {
    use super::*;

    /// Render diagnostics for a source, with the path the messages use.
    fn report(src: &str) -> (String, Vec<Diagnostic>) {
        let (_, diags) = check_module_recovering(src, &[]);
        (render_diagnostics("M.mod", src, &diags), diags)
    }

    fn messages(diags: &[Diagnostic]) -> Vec<String> {
        diags
            .iter()
            .filter(|d: &&Diagnostic| d.is_error())
            .map(|d| d.message.clone())
            .collect()
    }

    #[test]
    fn a_syntax_error_is_located_explained_and_underlined() {
        let src = "MODULE M;\n  VAR x: INTEGER;\nBEGIN\n  x := ;\nEND M.\n";
        let (text, diags) = report(src);
        assert_eq!(diags.len(), 1, "{text}");
        let d = &diags[0];
        assert_eq!(line_col(src, d.span.lo), (4, 8));
        // The message says what is wrong, the note what would fit, and the
        // rendering shows the line with a caret under the span.
        assert!(text.contains("M.mod:4:8: error: unexpected `;`"), "{text}");
        assert!(text.contains("= note: expected ident"), "{text}");
        assert!(text.contains("4 |   x := ;"), "{text}");
        assert!(text.contains("|        ^"), "{text}");
    }

    #[test]
    fn a_missing_end_is_reported_where_the_module_gives_out() {
        // No recovery point can absorb this: the parser has to say so, once.
        let src = "MODULE M;\n  VAR x: INTEGER;\nBEGIN\n  x := 1;\nIF x = 1 THEN x := 2\nEND M.\n";
        let (text, diags) = report(src);
        assert_eq!(messages(&diags).len(), 1, "{text}");
        assert!(text.contains("error: unexpected `M`"), "{text}");
        // `;` now leads the expected list (see `expected_note`), so this asserts
        // what the note must mention rather than the order it mentions it in.
        assert!(text.contains("expected ;") && text.contains("END"), "{text}");
        assert!(text.contains("6 | END M."), "{text}");
    }

    #[test]
    fn a_missing_separator_is_reported_with_both_notes() {
        // The commonest defect of all: two statements in a row. The reader needs
        // to know what would have fitted (`;` first) *and* what the recovery
        // discarded, because the discarded statement is the innocent one and its
        // own errors are never reported.
        let src = "MODULE M;\n  VAR x, y: INTEGER;\nBEGIN\n  x := 1\n  y := 2;\n  z := 3\nEND M.\n";
        let (text, diags) = report(src);
        assert_eq!(messages(&diags).len(), 2, "{text}");
        // the missing separator, located at the statement that followed it
        assert!(text.contains("M.mod:5:3: error: unexpected `y`"), "{text}");
        assert!(text.contains("= note: expected ;, "), "{text}");
        assert!(
            text.contains("= note: the parser skipped `y := 2` to carry on"),
            "{text}"
        );
        // and the check continues, so the real error after it is still found
        assert!(text.contains("M.mod:6:3: error: `z` is not declared"), "{text}");
    }

    #[test]
    fn a_missing_separator_in_a_declaration_names_what_followed() {
        // `CONST a = 1 b = 2;`: the parse cannot recover inside a declaration
        // list (DESIGN §5 D7), but the message still has to say what the parser
        // was doing when it gave up.
        let src = "MODULE M;\n  CONST a = 1\n        b = 2;\nBEGIN\nEND M.\n";
        let (text, diags) = report(src);
        assert_eq!(messages(&diags).len(), 1, "{text}");
        assert!(text.contains("M.mod:3:9: error: unexpected `b`"), "{text}");
        assert!(text.contains("= note: expected ;"), "{text}");
    }

    #[test]
    fn a_legal_identifier_named_error_survives_in_the_recovery_note() {
        // `Token` has no synthetic variants, so a dropped token is always input:
        // filtering by display string hid the name of the statement that was
        // discarded, and `error`/`Eof` are legal identifiers (report §3).
        let src = "MODULE M;\n  VAR x, error: INTEGER;\nBEGIN\n  x := 1\n  error := 2\nEND M.\n";
        let (text, _) = report(src);
        assert!(text.contains("the parser skipped `error := 2` to carry on"), "{text}");
    }

    #[test]
    fn one_occurrence_counts_as_one_use_even_when_resolved_twice() {
        // A type-`CASE` label is resolved for the arm's view and again for the
        // label check; only a new span is another use.
        let src = "MODULE M; TYPE R = RECORD END; P = POINTER TO R; VAR p: P;\nBEGIN CASE p OF Zz: END END M.\n";
        let (text, _) = report(src);
        assert!(!text.contains("further use of `Zz`"), "{text}");

        // Two genuine occurrences still count, including two labels of one arm
        // (which share the arm's span).
        let src = "MODULE M; VAR x: INTEGER;\nBEGIN x := Zz; x := Zz END M.\n";
        let (text, _) = report(src);
        assert!(text.contains("1 further use of `Zz` is suppressed"), "{text}");

        let src = "MODULE M; TYPE R = RECORD END; P = POINTER TO R; VAR p: P;\nBEGIN CASE p OF Zz, Zz: END END M.\n";
        let (text, _) = report(src);
        assert!(text.contains("1 further use of `Zz` is suppressed"), "{text}");
    }

    #[test]
    fn repeated_uses_of_one_undeclared_name_are_suppressed() {
        // PLAN.md W5.2: three uses of one undeclared name is one mistake, and a
        // second name is a second mistake.
        let src = "MODULE M;\n  VAR x: INTEGER;\nBEGIN\n  Zz := 1;\n  Zz := 2;\n  x := Zz\nEND M.\n";
        let (text, diags) = report(src);
        assert_eq!(messages(&diags).len(), 1, "{text}");
        assert!(text.contains("M.mod:4:3: error: `Zz` is not declared"), "{text}");
        assert!(
            text.contains("= note: 2 further uses of `Zz` are suppressed"),
            "{text}"
        );

        let src = "MODULE M;\n  VAR x: INTEGER;\nBEGIN\n  x := Aa;\n  x := Bb;\n  x := Aa\nEND M.\n";
        let (text, diags) = report(src);
        assert_eq!(messages(&diags).len(), 2, "{text}");
        assert!(text.contains("1 further use of `Aa` is suppressed"), "{text}");
        assert!(!text.contains("further use of `Bb`"), "{text}");
    }

    #[test]
    fn a_failed_type_or_constant_poisons_its_uses() {
        // The poison rule is deliberate, not incidental (DESIGN §10, PLAN.md
        // W5.2): a type that failed to resolve must not produce one error per
        // use of it.
        let src = "MODULE M;\n  TYPE T = Unknown;\n  VAR a: T;\nBEGIN\n  a := 1\nEND M.\n";
        let (text, diags) = report(src);
        assert_eq!(messages(&diags).len(), 1, "{text}");
        assert!(text.contains("`Unknown` is not declared"), "{text}");

        let src = "MODULE M;\n  CONST c = Zz;\n  VAR a: INTEGER;\n       TYPE U = ARRAY c OF INTEGER;\nBEGIN\n  a := c\nEND M.\n";
        let (_, diags) = report(src);
        assert_eq!(messages(&diags).len(), 1, "{diags:?}");

        // A missing field read twice is two mistakes, but neither assignment
        // adds a second error of its own.
        let src = "MODULE M;\n  TYPE R = RECORD n: INTEGER END;\n  VAR r: R;\nBEGIN\n  r.m := 1;\n  r.n := r.m\nEND M.\n";
        let (text, diags) = report(src);
        let msgs = messages(&diags);
        assert_eq!(msgs.len(), 2, "{text}");
        assert!(msgs.iter().all(|m| m.contains("has no field `m`")), "{msgs:?}");
    }

    #[test]
    fn the_error_limit_truncates_errors_and_never_warnings() {
        let mut diags = vec![
            Diagnostic::error(span_of(0, 1), "e1"),
            Diagnostic::new(sema::Severity::Warning, span_of(1, 2), "w1"),
            Diagnostic::error(span_of(2, 3), "e2"),
            Diagnostic::error(span_of(3, 4), "e3"),
        ];
        assert_eq!(limit_errors(&mut diags, 2), 1);
        let kept: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(kept, vec!["e1", "w1", "e2"], "warnings survive, order holds");

        // 0 is no limit, and a below-limit run is untouched.
        let mut diags = vec![
            Diagnostic::error(span_of(0, 1), "e1"),
            Diagnostic::error(span_of(1, 2), "e2"),
        ];
        assert_eq!(limit_errors(&mut diags, 0), 0);
        assert_eq!(diags.len(), 2);
        assert_eq!(limit_errors(&mut diags, 5), 0);
        assert_eq!(diags.len(), 2);
    }

    #[test]
    fn procedure_uses_are_tracked_by_identity_not_by_name() {
        // Two procedures of the same name at different levels: the one that is
        // called must not warn, and the one that is not must. A name-keyed check
        // would get this wrong in one direction or the other.
        let src = "MODULE M;\n\
  PROCEDURE P;\n    PROCEDURE Inner; BEGIN END Inner;\n  BEGIN Inner END P;\n\
  PROCEDURE Q;\n    PROCEDURE Inner; BEGIN END Inner;\n  BEGIN END Q;\n\
BEGIN P; Q END M.\n";
        let (text, diags) = report(src);
        let warnings: Vec<String> = diags
            .iter()
            .filter(|d| !d.is_error())
            .map(|d| d.message.clone())
            .collect();
        assert_eq!(warnings.len(), 1, "{text}");
        assert!(warnings[0].contains("procedure `Inner` is never used"), "{warnings:?}");
        // Q's Inner is on line 6 (indented four spaces, so column 15), P's on
        // line 3 — the caret has to be on the one that is not called.
        assert!(text.contains("M.mod:6:15: warning: procedure `Inner`"), "{text}");
    }

    #[test]
    fn the_procedure_warning_points_at_the_name_whatever_precedes_it() {
        // A comment and a blank line before the declaration must not move the
        // caret: `name_span` comes from the grammar, not from arithmetic on the
        // declaration's span.
        let src = "MODULE M;\n  (* a comment, and a blank line below *)\n\n  PROCEDURE Unused; BEGIN END Unused;\nBEGIN END M.\n";
        let (text, _) = report(src);
        assert!(text.contains("M.mod:4:13: warning: procedure `Unused` is never used"), "{text}");
        assert!(text.contains("4 |   PROCEDURE Unused;"), "{text}");
    }

    #[test]
    fn only_a_local_procedure_is_rejected_as_a_procedure_actual() {
        // The report's rule is narrow: a globally declared procedure and a
        // procedure *variable* are both legal actuals, and a procedure passed
        // where an INTEGER is expected is an ordinary type error (one message).
        let src = "MODULE M; TYPE F = PROCEDURE; VAR f: F;\n\
  PROCEDURE P0; BEGIN END P0;\n\
  PROCEDURE Taker(g: F); BEGIN END Taker;\n\
  PROCEDURE TakerInt(x: INTEGER); BEGIN END TakerInt;\n\
  PROCEDURE Outer;\n    PROCEDURE Inner; BEGIN END Inner;\n  BEGIN\n    Taker(P0);\n    f := P0;\n    TakerInt(P0);\n    Taker(Inner)\n  END Outer;\n\
BEGIN Outer END M.\n";
        let (text, diags) = report(src);
        let errors: Vec<String> = diags
            .iter()
            .filter(|d| d.is_error())
            .map(|d| d.message.clone())
            .collect();
        assert_eq!(errors.len(), 2, "{text}");
        assert!(
            errors[0].contains("does not match the formal") || errors[0].contains("INTEGER"),
            "{errors:?}"
        );
        assert!(
            errors[1].contains("cannot be passed as a procedure parameter"),
            "{errors:?}"
        );
    }

    #[test]
    fn a_constant_index_out_of_range_warns_and_still_traps() {
        // PLAN.md W7.2: a constant index the compiler can see is out of range is
        // a mistake worth naming, but the run-time check stays (report §8.1
        // leaves bounds checking to the implementation), so the warning must not
        // replace it.
        let src = "MODULE M;\n  CONST k = 7;\n  VAR a: ARRAY 4 OF INTEGER; i: INTEGER;\nBEGIN\n  a[10] := 1;\n  a[(-1)] := 2;\n  a[k] := 3;\n  a[i] := 4;\n  a[3] := 5\nEND M.\n";
        let (text, diags) = report(src);
        let warnings: Vec<String> = diags
            .iter()
            .filter(|d| !d.is_error())
            .map(|d| d.message.clone())
            .collect();
        assert_eq!(warnings.len(), 3, "{text}");
        assert!(warnings[0].contains("the index is 10, but this dimension has 4"), "{warnings:?}");
        assert!(warnings[1].contains("the index is -1"), "{warnings:?}");
        assert!(warnings[2].contains("the index is 7"), "{warnings:?}");
        assert!(diags.iter().all(|d| !d.is_error()), "{text}");

        // A compound index of a named constant, and the classic `a[LEN(a)]`,
        // where `LEN` is the predefined one.
        let src = "MODULE M;\n  CONST k = 7;\n  VAR a: ARRAY 4 OF INTEGER;\nBEGIN\n  a[k + 1] := 1;\n  a[-k] := 2;\n  a[LEN(a)] := 3\nEND M.\n";
        let (text, diags) = report(src);
        let warnings: Vec<String> = diags
            .iter()
            .filter(|d| !d.is_error())
            .map(|d| d.message.clone())
            .collect();
        assert_eq!(warnings.len(), 3, "{text}");
        assert!(warnings[0].contains("the index is 8"), "{warnings:?}");
        assert!(warnings[1].contains("the index is -7"), "{warnings:?}");
        assert!(warnings[2].contains("the index is 4"), "{warnings:?}");

        // A *shadowed* predefined name is not folded, so `ABS` here returns 0 and
        // there is nothing to warn about: the folder recognises a builtin by its
        // spelling, which is why calls are excluded.
        let src = "MODULE M;\n  VAR a: ARRAY 4 OF INTEGER;\n  PROCEDURE ABS(x: INTEGER): INTEGER; BEGIN RETURN 0 END ABS;\nBEGIN\n  a[ABS(10)] := 1\nEND M.\n";
        let (text, diags) = report(src);
        assert!(diags.iter().all(|d| !d.is_error()), "{text}");
        assert!(
            !diags.iter().any(|d| !d.is_error()),
            "a shadowed ABS is not the predefined one: {text}"
        );

        // The trap is still there for every index, constants included.
        let trap_src = "MODULE M;\n  CONST k = 7;\n  VAR a: ARRAY 4 OF INTEGER; i: INTEGER;\nBEGIN\n  a[10] := 1;\n  a[(-1)] := 2;\n  a[k] := 3;\n  a[i] := 4;\n  a[3] := 5\nEND M.\n";
        let c = match emit_c(trap_src, &[]) {
            Ok(Ok((c, _h))) => c,
            other => panic!("expected emitted C: {other:?}"),
        };
        assert_eq!(c.matches("rt_index(").count(), 5, "{c}");
    }

    #[test]
    fn an_uncalled_procedure_warns_and_a_used_one_does_not() {
        // ✔ row 54, the other half: the report has no way to mark a procedure as
        // deliberately unused. A call, a use as a value, an exported procedure
        // and a nested one are all covered here.
        let src = "MODULE M;\n  VAR p: PROCEDURE;\n  PROCEDURE Unused; BEGIN END Unused;\n  PROCEDURE Used; BEGIN END Used;\n  PROCEDURE AsValue; BEGIN END AsValue;\n  PROCEDURE Exported*; BEGIN END Exported;\n  PROCEDURE Outer;\n    PROCEDURE Inner; BEGIN END Inner;\n  BEGIN\n    Used;\n    p := AsValue\n  END Outer;\nBEGIN\n  Outer\nEND M.\n";
        let (text, diags) = report(src);
        let warnings: Vec<String> = diags
            .iter()
            .filter(|d| !d.is_error())
            .map(|d| d.message.clone())
            .collect();
        // `Unused` at module level, `Inner` nested: both warn. `Used`,
        // `AsValue`, `Exported` and `Outer` do not, so exactly two diagnostics.
        assert_eq!(warnings.len(), 2, "{text}\n{warnings:?}");
        assert!(
            warnings[0].contains("procedure `Unused` is never used"),
            "{warnings:?}"
        );
        assert!(
            warnings[1].contains("procedure `Inner` is never used"),
            "{warnings:?}"
        );
        // and the caret is on the *name*, not on the whole declaration
        assert!(
            text.contains("M.mod:3:13: warning: procedure `Unused` is never used"),
            "{text}"
        );
    }

    #[test]
    fn a_field_that_hides_an_inherited_field_warns() {
        // Report §6.3 permits it, so it stays legal — but `x.f` then means the
        // derived field, which is worth saying (DESIGN §13).
        let src = "MODULE M;\n  TYPE B = RECORD f: INTEGER END;\n       D = RECORD (B) f: CHAR END;\nBEGIN\nEND M.\n";
        let (text, diags) = report(src);
        assert!(diags.iter().all(|d| !d.is_error()), "{text}");
        assert_eq!(
            diags.iter().filter(|d| !d.is_error()).count(),
            1,
            "{text}"
        );
        assert!(text.contains("M.mod:3:"), "{text}");
        assert!(
            text.contains("warning: field `f` hides a field of the base record"),
            "{text}"
        );
        // and a field that does not hide anything stays silent
        let src = "MODULE M;\n  TYPE B = RECORD f: INTEGER END;\n       D = RECORD (B) g: CHAR END;\nBEGIN\nEND M.\n";
        let (text, diags) = report(src);
        assert!(diags.is_empty(), "{text}");
    }

    #[test]
    fn several_notes_are_all_rendered() {
        // One note per line: a diagnostic used to keep only the last one, so the
        // expected-token note vanished whenever a recovery added its own.
        let src = "MODULE M;\nBEGIN\n  x := 1\n  y := 2\nEND M.\n";
        let (text, _) = report(src);
        assert_eq!(text.matches("= note: ").count(), 2, "{text}");
    }

    #[test]
    fn a_lexical_error_names_the_character_and_what_to_do() {
        let src = "MODULE M;\nBEGIN\n  x := 1 $ 2\nEND M.\n";
        let (text, diags) = report(src);
        assert_eq!(messages(&diags).len(), 1, "{text}");
        assert!(text.contains("M.mod:3:10: error: illegal character '$'"), "{text}");
        assert!(text.contains("= note: not part of Oberon's vocabulary"), "{text}");
    }

    #[test]
    fn recovery_reports_later_errors_in_the_same_run() {
        // The broken statement is recovered from, so the *semantic* mistakes
        // after it are still found - which is the point of recovering at all.
        let src = "MODULE M;\n  VAR x: INTEGER;\nBEGIN\n  x := ;\n  y := 2;\n  z := 3\nEND M.\n";
        let msgs = messages(&report(src).1);
        assert!(msgs[0].contains("unexpected `;`"), "{msgs:?}");
        assert_eq!(msgs.len(), 3, "{msgs:?}");
        assert!(msgs[1].contains("`y` is not declared"), "{msgs:?}");
        assert!(msgs[2].contains("`z` is not declared"), "{msgs:?}");
    }

    #[test]
    fn several_syntax_errors_are_reported_in_one_run() {
        let src = "MODULE M;\n  VAR x, y: INTEGER;\nBEGIN\n  x := ;\n  y := ;\n  x := 1\nEND M.\n";
        let msgs = messages(&report(src).1);
        let syntax: Vec<&String> = msgs.iter().filter(|m| m.contains("unexpected")).collect();
        assert_eq!(syntax.len(), 2, "{msgs:?}");
        assert!(syntax[0].contains("unexpected `;`") && syntax[1].contains("unexpected `;`"));
    }

    #[test]
    fn semantic_errors_accumulate_with_their_own_locations() {
        let src = "MODULE M;\n  VAR x: INTEGER; r: REAL;\nBEGIN\n  x := r;\n  x := 1 + r;\n  x := f(1);\n  x := unknown\nEND M.\n";
        let (text, diags) = report(src);
        let msgs = messages(&diags);
        assert_eq!(msgs.len(), 4, "{text}");
        // each is located on its own line, in source order
        let lines: Vec<u32> = diags
            .iter()
            .filter(|d: &&Diagnostic| d.is_error())
            .map(|d| line_col(src, d.span.lo).0)
            .collect();
        assert_eq!(lines, vec![4, 5, 6, 7], "{text}");
    }

    #[test]
    fn the_common_mistakes_explain_themselves() {
        // Report §8.2.2's rule is the one that surprises people.
        let src = "MODULE M;\n  VAR x: INTEGER; r: REAL;\nBEGIN\n  x := r\nEND M.\n";
        let (text, _) = report(src);
        assert!(text.contains("cannot assign a REAL to an INTEGER"), "{text}");
        assert!(text.contains("use FLT or FLOOR"), "{text}");

        // And NIL, which is the other one.
        let src = "MODULE M;\n  VAR x: INTEGER;\nBEGIN\n  x := NIL\nEND M.\n";
        let (text, _) = report(src);
        assert!(text.contains("cannot assign NIL to an INTEGER"), "{text}");
        assert!(text.contains("pointer or a procedure variable"), "{text}");
    }

    #[test]
    fn one_mistake_is_reported_once() {
        // A call statement used to be looked up twice, so an unknown callee was
        // reported twice; and until PLAN.md W5.2 every *use* of an undeclared
        // name was reported, so two statements meant two diagnostics. Both
        // properties now hold at once: one name, one diagnostic, with the count
        // of the rest on it.
        let src = "MODULE M;\n  VAR x: INTEGER;\nBEGIN\n  Get(x)\nEND M.\n";
        let msgs = messages(&report(src).1);
        assert_eq!(msgs.len(), 1, "{msgs:?}");

        let src = "MODULE M;\n  VAR x: INTEGER;\nBEGIN\n  Get(x);\n  Get(x)\nEND M.\n";
        let (text, diags) = report(src);
        assert_eq!(messages(&diags).len(), 1, "{text}");
        assert!(text.contains("1 further use of `Get` is suppressed"), "{text}");
    }

    #[test]
    fn a_warning_is_rendered_as_a_warning() {
        // The import has to resolve for the unused-import *warning* to be the
        // only thing said about it.
        let iface = sym::parse("MODULE Texts\nVERSION 1\nPROC Write(ch: CHAR)\n").unwrap();
        let src = "MODULE M;\n  IMPORT Texts;\nBEGIN\nEND M.\n";
        let (_, diags) = check_module_recovering(src, &[iface]);
        let text = render_diagnostics("M.mod", src, &diags);
        assert!(diags.iter().any(|d| !d.is_error()), "{text}");
        assert!(text.contains("M.mod:2:10: warning: import `Texts` is never used"), "{text}");
    }

    #[test]
    fn the_source_line_is_echoed_even_when_the_span_is_at_the_end() {
        // An empty statement slot at the end of a module body: the caret has
        // nowhere to point beyond the line, and must not panic.
        let src = "MODULE M;\nBEGIN\n  ;\nEND M.\n";
        let (text, _) = report(src);
        assert!(text.is_empty() || text.contains("M.mod:"), "{text}");
    }
}

#[cfg(test)]
mod check_tests {
    use super::*;
    use crate::sym::Interface;

    /// A fixture interface. The version line is part of the format but not part
    /// of what these fixtures are about, so it is supplied here (PLAN.md W3.2).
    fn iface(text: &str) -> Interface {
        let text = match text.split_once('\n') {
            Some((first, rest)) if first.starts_with("MODULE ") => {
                format!("{first}\nVERSION {}\n{rest}", sym::FORMAT_VERSION)
            }
            _ => text.to_string(),
        };
        sym::parse(&text).unwrap_or_else(|e| panic!("fixture interface: {e}"))
    }

    /// The interfaces the report's own example modules need. `Texts.Writer` is
    /// a real record here, and `Oberon.Log` is typed through `Texts`, which is
    /// what makes the interface import list load-bearing.
    fn report_interfaces() -> Vec<Interface> {
        vec![
            iface("MODULE Texts\n\
TYPE Buffer = ARRAY 256 OF CHAR\n\
TYPE Writer = RECORD buf*: Buffer END\n\
PROC OpenWriter(VAR w: Writer)\n\
PROC Write(w: Writer; ch: CHAR)\n\
PROC WriteLn(w: Writer)\n\
PROC Append(p: Writer; b: Buffer)\n"),
            iface("MODULE Oberon\nIMPORT Texts\nVAR Log: Texts.Writer\n"),
            iface("MODULE Other\n"),
            iface("MODULE Strings\n"),
        ]
    }

    fn run(src: &str, interfaces: &[Interface]) -> Vec<Diagnostic> {
        check_module(src, interfaces)
            .unwrap_or_else(|e| panic!("must parse: {e:?}\n  source: {src}"))
            .1
    }

    fn errors(src: &str, interfaces: &[Interface]) -> Vec<String> {
        run(src, interfaces)
            .into_iter()
            .filter(|d: &Diagnostic| d.is_error())
            .map(|d| d.message)
            .collect()
    }

    fn warnings(src: &str, interfaces: &[Interface]) -> Vec<String> {
        run(src, interfaces)
            .into_iter()
            .filter(|d| !d.is_error())
            .map(|d| d.message)
            .collect()
    }

    #[test]
    fn a_procedure_name_stays_visible_for_the_rest_of_its_scope() {
        // The other half of the declaration-order rule (report §4): a procedure's
        // name is visible to the declarations after it and to the body — here a
        // procedure named like a *predefined* type shadows it for both, so
        // `Take(INTEGER)` passes the procedure, not the type.
        let src = "MODULE M;\n  TYPE F = PROCEDURE;\n\
  PROCEDURE Take(f: F); BEGIN END Take;\n\
  PROCEDURE INTEGER;\n  BEGIN\n  END INTEGER;\n\
BEGIN\n  Take(INTEGER)\nEND M.\n";
        let e = errors(src, &[]);
        assert!(e.is_empty(), "`INTEGER` denotes the procedure here: {e:#?}");
    }

    #[test]
    fn a_later_sibling_procedure_does_not_shadow_a_type() {
        // Report §4: a declaration is visible from where it stands. Inside `Use`,
        // the module type `T` is still the type, even though a *later* sibling
        // procedure is named `T` — classifying that as a call would reject a legal
        // guard.
        let src = "MODULE M;\n  TYPE R = RECORD a: INTEGER END;\n       T = RECORD (R) f: INTEGER END;\n\
  VAR p: POINTER TO R;\n\
  PROCEDURE Outer;\n    PROCEDURE Use;\n    BEGIN\n      p(T).f := 1\n    END Use;\n\
    PROCEDURE T;\n    BEGIN\n    END T;\n  BEGIN Use END Outer;\n\
BEGIN Outer END M.\n";
        let e = errors(src, &[]);
        assert!(e.is_empty(), "`T` is still the type inside `Use`: {e:#?}");
    }

    #[test]
    fn an_inner_declaration_shadows_a_type_name_and_an_import_alias() {
        // R17: the reclassification pass decided guard-vs-call and
        // module-qualification from module-wide name sets, so an inner
        // declaration did not shadow them. Both cases below were rejected before
        // the pass became scope-aware: `Texts.f` was folded into a qualified name
        // ("`Texts` is not a module") and `Q(T)` was read as a type guard
        // ("`T` is not a type").
        let shadowed_alias = "MODULE M;\n  IMPORT Texts;\n  TYPE R = RECORD f: INTEGER END;\n\
  PROCEDURE Use;\n    VAR Texts: R;\n  BEGIN\n    Texts.f := 1\n  END Use;\nBEGIN Use END M.\n";
        let e = errors(shadowed_alias, &report_interfaces());
        assert!(e.is_empty(), "a local `Texts` shadows the import alias: {e:#?}");

        let shadowed_type = "MODULE M;\n  TYPE T = RECORD a: INTEGER END;\n       P = POINTER TO T;\n\
  VAR p: P;\n  PROCEDURE Q(x: INTEGER): INTEGER; BEGIN RETURN x END Q;\n\
  PROCEDURE Use;\n    VAR T: INTEGER;\n  BEGIN\n    p := NIL;\n    T := Q(T)\n  END Use;\nBEGIN Use END M.\n";
        let e = errors(shadowed_type, &[]);
        assert!(e.is_empty(), "a local `T` shadows the type: {e:#?}");
    }

    /// Report §11.1's SYSTEM, in full (W4.1): every entry of the table's portable
    /// six, the entry the report does not define, both spellings of the import
    /// alias, and the qualified type name in `SIZE`.
    #[test]
    fn system_entries_check_clean() {
        let src = "MODULE M;\n  IMPORT SYSTEM, S := SYSTEM;\n\
  TYPE R = RECORD a: INTEGER END; P = POINTER TO R; Arr = ARRAY 4 OF INTEGER;\n\
  VAR i, j: INTEGER; r: R; c: CHAR; b: BOOLEAN; x: REAL; a: Arr; p: P;\n\
  PROCEDURE P0(VAR v: R);\n    VAR w: R;\n  BEGIN\n\
    i := SYSTEM.ADR(v);\n\
    i := S.ADR(w);\n\
    i := SYSTEM.SIZE(R);\n\
    i := SYSTEM.SIZE(INTEGER);\n\
    i := SYSTEM.SIZE(Arr);\n\
    b := SYSTEM.BIT(i, 31)\n\
  END P0;\n\
BEGIN\n\
  SYSTEM.PUT(SYSTEM.ADR(i), 1);\n\
  SYSTEM.GET(SYSTEM.ADR(i), j);\n\
  SYSTEM.PUT(SYSTEM.ADR(c), c);\n\
  SYSTEM.GET(SYSTEM.ADR(c), c);\n\
  SYSTEM.PUT(SYSTEM.ADR(b), b);\n\
  SYSTEM.GET(SYSTEM.ADR(b), b);\n\
  SYSTEM.PUT(SYSTEM.ADR(x), x);\n\
  SYSTEM.GET(SYSTEM.ADR(x), x);\n\
  SYSTEM.COPY(SYSTEM.ADR(a), SYSTEM.ADR(a), SYSTEM.SIZE(R) DIV 4);\n\
  NEW(p);\n\
  SYSTEM.COPY(SYSTEM.ADR(r), SYSTEM.ADR(p^), 2);\n\
  P0(r)\n\
END M.\n";
        let e = errors(src, &[]);
        // `SIZE`'s one-argument rule (`Arr` is an *array type name* here, not a
        // value: `SIZE(T)` takes any type) and both aliases are the point.
        assert!(e.is_empty(), "the report's SYSTEM entries must check: {e:#?}");
        let w = warnings(src, &[]);
        assert!(w.is_empty(), "every import is used, and so is P0: {w:#?}");
    }

    /// W8.10: the default dialect is the report's, so a reserved word spelled in
    /// lower case is an identifier there — which is what makes the switch, rather
    /// than the change, the safe direction.
    #[test]
    fn lower_case_keywords_need_the_dialect() {
        let src = "module M;\n  var i: INTEGER;\nbegin\n  i := 1\nend M.\n";
        let (parsed, report) = parse_with_diagnostics(src);
        assert!(parsed.is_none(), "the report dialect cannot read `module`");
        assert!(
            report
                .iter()
                .any(|d| d.notes.iter().any(|n| n.contains("MODULE"))),
            "and the note names what it wanted: {report:#?}"
        );

        let lower = parse_module_dialect(src, Dialect::Lower);
        assert!(lower.is_ok(), "the lower dialect accepts it: {lower:?}");

        // Identifiers stay case-sensitive...
        let mixed = parse_module_dialect(
            "module M;\n  var Foo, foo: INTEGER;\nbegin\n  Foo := 1; foo := 2\nend M.\n",
            Dialect::Lower,
        );
        assert!(mixed.is_ok(), "Foo and foo remain distinct: {mixed:?}");

        // ... and the predeclared *identifiers* are not folded with them: a
        // declaration named `integer` is a different name from `INTEGER`, in
        // either dialect. (Aliasing those is the separate, additive half W8.10
        // describes; it is not in this change.)
        let shadow = parse_module_dialect(
            "module M;\n  var integer: INTEGER;\nbegin\n  integer := 1\nend M.\n",
            Dialect::Lower,
        );
        assert!(shadow.is_ok(), "a lowercase `integer` is an identifier: {shadow:?}");
    }

    /// W8.10: when a reserved word *is* a collision — the dialect folds case, and an
    /// identifier was spelled like a keyword — the diagnostic says why, because
    /// otherwise "unexpected `BEGIN`" is a puzzle.
    #[test]
    fn a_folded_collision_explains_itself() {
        let src = "module M;\n  var begin: INTEGER;\nbegin\n  begin := 1\nend M.\n";
        let (_, diags) = parse_with_diagnostics_dialect(src, Dialect::Lower);
        let noted = diags.iter().any(|d| {
            d.notes.iter().any(|n| n.contains("folds reserved-word case"))
        });
        assert!(noted, "a folded collision must name the dialect: {diags:#?}");

        // An honestly upper-case keyword gets no such note: it is a reserved word
        // in either dialect, so nothing about the dialect explains it.
        // Spelled the way the report spells it: a reserved word in either dialect,
        // so nothing about the dialect explains it.
        let (_, upper) = parse_with_diagnostics_dialect(
            "MODULE M;\n  VAR BEGIN: INTEGER;\nBEGIN\n  BEGIN := 1\nEND M.\n",
            Dialect::Lower,
        );
        assert!(
            !upper
                .iter()
                .any(|d| d.notes.iter().any(|n| n.contains("folds reserved-word case"))),
            "{upper:#?}"
        );
    }

    /// W8.2: `MIN(T)`/`MAX(T)` name the extreme of an ordinal type, and the value
    /// is a constant — in a constant declaration as much as in a statement.
    #[test]
    fn min_and_max_are_constants_of_ordinal_types() {
        let src = "MODULE M;\n  CONST Low = MIN(INTEGER); High = MAX(INTEGER);\n\
  VAR i: INTEGER; b: BYTE; c: CHAR; r: REAL;\n\
BEGIN\n  i := MIN(INTEGER); i := MAX(BYTE); i := MIN(CHAR);\n\
  i := MAX(BYTE) - MIN(BYTE); c := CHR(MAX(CHAR))\nEND M.\n";
        let e = errors(src, &[]);
        assert!(e.is_empty(), "the extremes are constants: {e:#?}");

        // A type without an extreme is refused rather than given one, and so is
        // an argument that is an expression rather than a type name.
        let bad = errors("MODULE M;\n  VAR i: INTEGER; r: REAL;\nBEGIN\n  i := MIN(REAL)\nEND M.\n", &[]);
        assert!(
            bad.iter().any(|d| d.contains("defined for INTEGER, BYTE and CHAR")),
            "{bad:#?}"
        );
        let bad = errors("MODULE M;\n  VAR i: INTEGER;\nBEGIN\n  i := MIN(i)\nEND M.\n", &[]);
        assert!(bad.iter().any(|d| d.contains("must be a type name")), "{bad:#?}");
    }

    /// W8.2: a program that declares its own `MIN` shadows the predefined one —
    /// the scoping rule every predeclared name already has.
    #[test]
    fn a_declared_min_shadows_the_predefined_one() {
        let src = "MODULE M;\n  VAR i: INTEGER;\n\
  PROCEDURE MIN(x: INTEGER): INTEGER;\n  BEGIN RETURN x + 1 END MIN;\n\
BEGIN\n  i := MIN(41)\nEND M.\n";
        let e = errors(src, &[]);
        assert!(e.is_empty(), "a user MIN takes a value, not a type: {e:#?}");
    }

    /// W8.1: `CASE … ELSE` is report §9.5's default arm under a keyword LALR(1)
    /// can parse, and two defaults are one answer too many.
    #[test]
    fn case_else_is_the_default_arm_and_there_is_only_one() {
        let src = "MODULE M;\n  VAR i: INTEGER;\nBEGIN\n\
  CASE i OF 1: i := 1 | 2: i := 2 ELSE i := 3 END;\n\
  CASE i OF 1: i := 1 ELSE i := 3 END\nEND M.\n";
        let e = errors(src, &[]);
        assert!(e.is_empty(), "ELSE is the default arm: {e:#?}");

        let two = errors(
            "MODULE M;\n  VAR i: INTEGER;\nBEGIN\n  CASE i OF 1: i := 1 | ELSE i := 2 END\nEND M.\n",
            &[],
        );
        assert!(
            two.iter().any(|d| d.contains("two default arms")),
            "{two:#?}"
        );
    }

    /// `SIZE(Texts.Writer)` is a *type* from another module: the qualified name
    /// has to resolve, and it counts as a use of the import that names it.
    #[test]
    fn system_size_takes_an_imported_type_name() {
        let src = "MODULE M;\n  IMPORT SYSTEM, Texts;\n  VAR i: INTEGER;\n\
  BEGIN i := SYSTEM.SIZE(Texts.Writer) END M.\n";
        let all = run(src, &report_interfaces());
        let e: Vec<&String> = all.iter().filter(|d| d.is_error()).map(|d| &d.message).collect();
        assert!(e.is_empty(), "SIZE of an imported type must check: {e:#?}");
        let w: Vec<&String> = all.iter().filter(|d| !d.is_error()).map(|d| &d.message).collect();
        assert!(w.is_empty(), "the qualified name is a use of `Texts`: {w:#?}");
    }

    /// SYSTEM is built in, so an interface file *named* SYSTEM is not what the
    /// import resolves to (DESIGN §9.10): the report's entries are there, and
    /// anything else is refused by name rather than looked up in that file.
    #[test]
    fn system_import_is_not_an_interface_lookup() {
        let interfaces = vec![iface("MODULE SYSTEM\nPROC Nope\n")];
        let src = "MODULE M;\n  IMPORT SYSTEM;\n  VAR i: INTEGER;\n\
  BEGIN i := SYSTEM.ADR(i) END M.\n";
        let e = errors(src, &interfaces);
        assert!(e.is_empty(), "the builtin entries must resolve: {e:#?}");

        let src = "MODULE M;\n  IMPORT SYSTEM;\n  VAR i: INTEGER;\n\
  BEGIN i := SYSTEM.Nope(1) END M.\n";
        let e = errors(src, &interfaces);
        assert_eq!(e.len(), 1, "{e:#?}");
        assert!(e[0].contains("SYSTEM.Nope is not implemented"), "{e:#?}");
    }

    /// R17 applies to this pass's SYSTEM set as well: a declaration named SYSTEM
    /// shadows the import alias, so `SYSTEM.SIZE` there is a field select on that
    /// declaration. The test is the *absence* of a diagnostic: report §11.1's
    /// entry written without its argument list would be row 51's error, so
    /// resolving it as the record's field is the only way this checks clean.
    #[test]
    fn an_inner_declaration_shadows_the_system_alias() {
        let src = "MODULE M;\n  IMPORT SYSTEM;\n  TYPE T = RECORD SIZE: INTEGER END;\n\
  VAR i: INTEGER;\n\
  PROCEDURE Use;\n    VAR SYSTEM: T;\n  BEGIN\n    SYSTEM.SIZE := 1;\n    i := SYSTEM.SIZE\n  END Use;\n\
BEGIN Use END M.\n";
        let e = errors(src, &[]);
        assert!(e.is_empty(), "the local SYSTEM is a record: {e:#?}");
    }

    #[test]
    fn report_out_module_checks_clean() {
        let e = errors(OUT_MODULE, &report_interfaces());
        assert!(e.is_empty(), "the report's own module must check: {e:#?}");
    }

    #[test]
    fn kitchen_sink_checks_clean() {
        let e = errors(KITCHEN_SINK, &report_interfaces());
        assert!(e.is_empty(), "every report construct must check: {e:#?}");
        // `IMPORT M := Other, Strings;` is never used: one warning each.
        let w = warnings(KITCHEN_SINK, &report_interfaces());
        assert_eq!(w.len(), 2, "{w:#?}");
    }

    /// One case per catalogue row of DESIGN.md §8.5. The row number is in the
    /// test name so a failure points straight at the rule.
    #[test]
    fn catalogue_rows_report_what_they_should() {
        let cases: &[(u32, &str, &str)] = &[
            (1, "MODULE M; VAR x: INTEGER; BEGIN y := 1 END M.", "`y` is not declared"),
            (2, "MODULE M; VAR x, x: INTEGER; END M.", "`x` is already declared"),
            // row 3 has two shapes: a qualifier that is not a module, and a
            // module referenced without importing it (which is simply an
            // undeclared name, because nothing folds the qualifier then).
            (3, "MODULE M; TYPE T = INTEGER.N; BEGIN END M.", "is not a module"),
            (3, "MODULE M; VAR x: INTEGER; BEGIN x := Texts.N END M.", "`Texts` is not declared"),
            (4, "MODULE M; IMPORT Texts; VAR c: CHAR; BEGIN c := Texts.Nope END M.", "is not exported by module `Texts`"),
            (5, "MODULE M; IMPORT M; BEGIN END M.", "cannot import itself"),
            (
                6,
                "MODULE M; TYPE R = RECORD x: INTEGER END;\n\
                 PROCEDURE P(v: R); BEGIN v.x := 1 END P;\n\
                 BEGIN END M.",
                "structured value parameter",
            ),
            (7, "MODULE M; VAR i: INTEGER; r: REAL; BEGIN i := r END M.", "cannot assign a REAL"),
            (8, "MODULE M; VAR i: INTEGER; BEGIN i := NIL END M.", "cannot assign NIL"),
            (9, "MODULE M; VAR a: ARRAY 3 OF CHAR; BEGIN a := \"abcd\" END M.", "cannot assign a 4-character string"),
            (10, "MODULE M; VAR a: ARRAY 3 OF INTEGER; i: INTEGER; BEGIN i := a[1, 2] END M.", "1 dimension(s) but 2"),
            (11, "MODULE M; VAR a: ARRAY 3 OF INTEGER; r: REAL; BEGIN a[r] := 1 END M.", "index must be an INTEGER"),
            (12, "MODULE M; VAR i: INTEGER; BEGIN i := i^ END M.", "is not a pointer"),
            (13, "MODULE M; VAR i: INTEGER; BEGIN i := i.x END M.", "is not a record"),
            (14, "MODULE M; TYPE R = RECORD x: INTEGER END; VAR r: R; BEGIN r.y := 1 END M.", "has no field `y`"),
            (
                15,
                "MODULE M; TYPE A = RECORD END; B = RECORD (A) END; VAR a: A; x: A;\n\
                 BEGIN x := a(B) END M.",
                "applies to a pointer or a VAR record parameter",
            ),
            (
                16,
                "MODULE M; TYPE R = RECORD END; S = RECORD END; P = POINTER TO R;\n\
                 VAR p: P; r: R;\n\
                 BEGIN r := p(S) END M.",
                "must be an extension",
            ),
            (17, "MODULE M; TYPE R = RECORD END; VAR r: R; BEGIN IF r IS R THEN END END M.", "applies to a pointer or a VAR record parameter"),
            (18, "MODULE M; VAR i: INTEGER; r: REAL; BEGIN i := i + r END M.", "must have the same type"),
            (19, "MODULE M; VAR r: REAL; i: INTEGER; BEGIN i := r DIV 2.0 END M.", "DIV and MOD apply to integer operands only"),
            (20, "MODULE M; VAR i: INTEGER; BEGIN i := i OR i END M.", "applies to BOOLEAN operands"),
            (21, "MODULE M; VAR i: INTEGER; s: SET; BEGIN i := s * i END M.", "operands of this operator must have the same type"),
            (22, "MODULE M; VAR i: INTEGER; b: BOOLEAN; BEGIN IF i < b THEN END END M.", "ordering relations apply to"),
            (23, "MODULE M; VAR s: SET; BEGIN IF 1.5 IN s THEN END END M.", "needs an INTEGER x and a SET"),
            (24, "MODULE M; VAR i: INTEGER; BEGIN IF i THEN END END M.", "guard of an IF must be a BOOLEAN"),
            (25, "MODULE M; PROCEDURE P(x: INTEGER); BEGIN END P; BEGIN P(1, 2) END M.", "expected 1 actual parameter(s), found 2"),
            (26, "MODULE M; PROCEDURE P(x: INTEGER); BEGIN END P; BEGIN P(1.0) END M.", "does not match the formal"),
            (27, "MODULE M; PROCEDURE P(VAR x: INTEGER); BEGIN END P; BEGIN P(1) END M.", "VAR parameter needs an actual that is a variable"),
            (28, "MODULE M; VAR i: INTEGER; BEGIN i(1) END M.", "is not a procedure"),
            (
                29,
                // `VAR` must precede the procedure declarations (report 10):
                // the report's declaration order is CONST, TYPE, VAR, procedures.
                "MODULE M; VAR i: INTEGER;\n\
                 PROCEDURE F(x: INTEGER): INTEGER; BEGIN RETURN x END F;\n\
                 BEGIN i := F(1)(2) END M.",
                "cannot be applied twice",
            ),
            (30, "MODULE M; PROCEDURE P(x: INTEGER); BEGIN END P; BEGIN P END M.", "actual parameter list is required"),
            (31, "MODULE M; PROCEDURE F(): INTEGER; BEGIN RETURN 1 END F; BEGIN F() END M.", "must be used for its value"),
            (32, "MODULE M; VAR i: INTEGER; BEGIN FOR i := 1 TO 2 DO i := 3 END END M.", "control variable of an enclosing FOR"),
            (33, "MODULE M; VAR r: REAL; i: INTEGER; BEGIN FOR i := r TO 2 DO END END M.", "initial value of a FOR must be an INTEGER"),
            (34, "MODULE M; VAR i: INTEGER; BEGIN FOR i := 1 TO 2 BY 0 DO END END M.", "must not be zero"),
            (35, "MODULE M; VAR i: INTEGER; BEGIN CASE i OF \"ab\": END END M.", "single character"),
            (36, "MODULE M; VAR i: INTEGER; BEGIN CASE i OF 1: | 1: END END M.", "overlaps the earlier range"),
            // and a range covers its interior, not just its endpoints
            (36, "MODULE M; VAR i: INTEGER; BEGIN CASE i OF 1..3: | 2: END END M.", "overlaps the earlier range"),
            (
                37,
                "MODULE M; TYPE R = RECORD END; S = RECORD END; P = POINTER TO R;\n\
                 VAR p: P; BEGIN CASE p OF S: END END M.",
                "not an extension of the case variable's type",
            ),
            (38, "MODULE M; TYPE R = RECORD END; VAR r: R; BEGIN CASE r OF R: END END M.", "type CASE needs a pointer or a VAR record parameter"),
            (40, "MODULE M; CONST c = 1 DIV 0; BEGIN END M.", "division by zero in a constant expression"),
            (41, "MODULE M; CONST n = 0; TYPE T = ARRAY n OF INTEGER; BEGIN END M.", "array length must be positive"),
            (42, "MODULE M; TYPE T = RECORD (INTEGER) END; BEGIN END M.", "base type of a record must be a record"),
            (43, "MODULE M; TYPE R = RECORD x: INTEGER; x: REAL END; BEGIN END M.", "field `x` is declared twice"),
            (44, "MODULE M; TYPE P = POINTER TO INTEGER; BEGIN END M.", "base type of a pointer must be a record"),
            (45, "MODULE M; TYPE R = RECORD END; P = PROCEDURE (): R; BEGIN END M.", "result type of a procedure may not be a record"),
            (46, "MODULE M; PROCEDURE F(): INTEGER; BEGIN END F; BEGIN END M.", "must contain a RETURN clause"),
            (46, "MODULE M; PROCEDURE P; BEGIN RETURN 1 END P; BEGIN END M.", "may not return a value"),
            (47, "MODULE M; PROCEDURE P; BEGIN END Q; BEGIN END M.", "identifier after END is `Q`"),
            (48, "MODULE M; PROCEDURE P; VAR x*: INTEGER; BEGIN END P; BEGIN END M.", "export mark is only allowed at module scope"),
            // report §6.3: a field's `*` marks it public, so it is *not* an error.
            (
                48,
                "MODULE M; TYPE R = RECORD x*: INTEGER END; BEGIN END M.",
                "PLACEHOLDER-NEVER-MATCHES",
            ),
            (49, "MODULE M; PROCEDURE P; BEGIN END P; BEGIN P(1) END M.", "expected 0 actual parameter(s), found 1"),
            (
                50,
                "MODULE M; VAR f: PROCEDURE;\n\
                 PROCEDURE Outer; PROCEDURE Inner; BEGIN END Inner;\n\
                 BEGIN f := Inner END Outer;\n\
                 BEGIN END M.",
                "cannot be assigned to a procedure variable",
            ),
            // row 50's other half: the report's rule is about the *value*, so it
            // holds for an actual parameter too (report §10.1, "must be either a
            // procedure declared globally, or a variable of that procedure
            // type").
            (
                50,
                "MODULE M; TYPE F = PROCEDURE;\n\
                 PROCEDURE Taker(f: F); BEGIN END Taker;\n\
                 PROCEDURE Outer; PROCEDURE Inner; BEGIN END Inner;\n\
                 BEGIN Taker(Inner) END Outer;\n\
                 BEGIN END M.",
                "cannot be passed as a procedure parameter",
            ),
            (51, "MODULE M; VAR i: INTEGER; BEGIN i := ABS END M.", "cannot be used as a value"),
            // Row 51 covers report §11.1's entries too: they are predeclared
            // procedures as well, and one the report lists as
            // implementation-specific is refused by row 53's message instead.
            (
                51,
                "MODULE M; IMPORT SYSTEM; VAR i: INTEGER; BEGIN i := SYSTEM.ADR END M.",
                "`SYSTEM.ADR` is a predeclared procedure: it cannot be used as a value",
            ),
            (52, "MODULE M; VAR s: SET; BEGIN s := {32} END M.", "outside 0..31"),
            // Row 53 is the by-name refusal (W4.1): the report's table defines
            // six entries, and the extras *it* lists (`VAL`, `ADC`, `SBC`, `UML`,
            // `COND`, `LED`) plus anything else a program spells are refused with
            // the name the program used, never silently.
            (
                53,
                "MODULE M; IMPORT SYSTEM; VAR i: INTEGER; BEGIN SYSTEM.ADC(i, i) END M.",
                "SYSTEM.ADC is not implemented",
            ),
            (
                53,
                "MODULE M; IMPORT SYSTEM; VAR i: INTEGER; BEGIN i := SYSTEM.H(i) END M.",
                "SYSTEM.H is not implemented",
            ),
            // Rows 55-59 are the entries' argument rules (W4.1).
            (
                55,
                "MODULE M; IMPORT SYSTEM; VAR i: INTEGER; BEGIN i := SYSTEM.SIZE(i) END M.",
                "the argument of SYSTEM.SIZE must be a type name",
            ),
            (
                56,
                "MODULE M; IMPORT SYSTEM; VAR i: INTEGER; BEGIN i := SYSTEM.ADR(1) END M.",
                "the argument of SYSTEM.ADR must be a variable",
            ),
            (
                57,
                "MODULE M; IMPORT SYSTEM; TYPE R = RECORD a: INTEGER END;\n\
                 VAR i: INTEGER; r: R;\n\
                 BEGIN SYSTEM.GET(i, r) END M.",
                "argument 2 of SYSTEM.GET must be of a basic type",
            ),
            (
                57,
                "MODULE M; IMPORT SYSTEM; TYPE R = RECORD a: INTEGER END;\n\
                 VAR i: INTEGER; r: R;\n\
                 BEGIN SYSTEM.PUT(i, r) END M.",
                "argument 2 of SYSTEM.PUT must be of a basic type",
            ),
            (
                58,
                "MODULE M; IMPORT SYSTEM; CONST K = 7; VAR i: INTEGER;\n\
                 BEGIN SYSTEM.GET(i, K) END M.",
                "read-only",
            ),
            (
                59,
                "MODULE M; IMPORT SYSTEM; VAR i: INTEGER; BEGIN SYSTEM.COPY(i, i) END M.",
                "SYSTEM.COPY takes 3 argument(s), but 2 were given",
            ),
            // Row 30's shape reaches SYSTEM's proper entries as well.
            (
                30,
                "MODULE M; IMPORT SYSTEM; BEGIN SYSTEM.GET END M.",
                "`SYSTEM.GET` needs an actual parameter list",
            ),
            // ... and row 31's its functions: a call, which the statement path
            // asks about by name; and the bare name, which no argument list
            // follows at all.
            (
                31,
                "MODULE M; IMPORT SYSTEM; VAR i: INTEGER; BEGIN SYSTEM.ADR(i) END M.",
                "a function procedure must be used for its value, not as a statement",
            ),
            (
                31,
                "MODULE M; IMPORT SYSTEM; VAR i: INTEGER; BEGIN SYSTEM.ADR END M.",
                "`SYSTEM.ADR` is a function procedure: its value must be used",
            ),
            (
                39,
                "MODULE M; TYPE R = RECORD END; P = POINTER TO R; VAR p: P;\n\
                 BEGIN CASE p OF P: END END M.",
                "no default arm",
            ),
        ];
        let interfaces = report_interfaces();
        // A row may assert the presence of a diagnostic, or - with the
        // sentinel - that a construct is accepted.
        let mut failures: Vec<String> = Vec::new();
        for (row, src, want) in cases {
            let all = run(src, &interfaces);
            if *want == "PLACEHOLDER-NEVER-MATCHES" {
                let unexpected: Vec<&String> = all
                    .iter()
                    .filter(|d: &&Diagnostic| d.is_error())
                    .map(|d| &d.message)
                    .collect();
                assert!(
                    unexpected.is_empty(),
                    "row {row} must produce no error\n  source: {src}\n  got: {unexpected:#?}"
                );
                continue;
            }
            let matched = all.iter().any(|d| d.message.contains(want));
            if !matched {
                failures.push(format!(
                    "row {row}: expected a diagnostic containing {want:?}\n  source: {src}\n  got: {:#?}",
                    all.iter().map(|d| &d.message).collect::<Vec<_>>()
                ));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn the_unused_import_warning_is_row_54() {
        let w = warnings("MODULE M; IMPORT Texts; BEGIN END M.", &report_interfaces());
        assert_eq!(w.len(), 1, "{w:#?}");
        assert!(w[0].contains("never used"), "{w:#?}");
        // A used import is not reported.
        let w = warnings(
            "MODULE M; IMPORT Texts; VAR w: Texts.Writer; BEGIN Texts.OpenWriter(w) END M.",
            &report_interfaces(),
        );
        assert!(w.is_empty(), "{w:#?}");
    }

    #[test]
    fn diagnostics_carry_usable_spans() {
        let src = "MODULE M; VAR i: INTEGER; BEGIN i := NIL END M.";
        let diags = errors(src, &[]);
        assert_eq!(diags.len(), 1);
        let (line, col) = line_col(src, run(src, &[])[0].span.lo);
        assert_eq!((line, col), (1, 33), "the span points at the assignment target");
    }
}
