//! Oberon-07 front end prototype: lexer + LALRPOP grammar + AST.
//!
//! This crate exists to *verify* the design in ../../DESIGN.md: that the
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

lalrpop_util::lalrpop_mod!(pub oberon);

use ast::Module;
use lexer::{Lexer, LexicalError, Token};
use lalrpop_util::ParseError;

pub type ParseErr<'input> = ParseError<usize, Token<'input>, LexicalError>;

pub fn parse_module<'input>(input: &'input str) -> Result<Module, ParseErr<'input>> {
    oberon::CompilationUnitParser::new().parse(Lexer::new(input))
}

pub use sema::Diagnostic;

/// Check a module and, if it is clean, emit its C translation unit and header.
/// Returns `Err` with the diagnostics when the module does not check: the
/// emitter assumes the checks passed and would otherwise emit nonsense.
/// The interface file text for a module, or `None` when it does not check.
pub fn emit_interface(input: &str, interfaces: &[sym::Interface]) -> Option<String> {
    let mut m = parse_module(input).ok()?;
    let sema = sema::Sema::check_owned(&mut m, interfaces);
    if sema.diagnostics().iter().any(Diagnostic::is_error) {
        return None;
    }
    Some(sym::render(&cgen::interface_of(&m, &sema.types, &sema)))
}

pub fn emit_c<'input>(
    input: &'input str,
    interfaces: &[sym::Interface],
) -> Result<Result<(String, String), Vec<Diagnostic>>, ParseErr<'input>> {
    let mut m = parse_module(input)?;
    let sema = sema::Sema::check_owned(&mut m, interfaces);
    if sema.diagnostics().iter().any(Diagnostic::is_error) {
        return Ok(Err(sema.diags.clone()));
    }
    let (c, h) = cgen::emit_module(&m, &sema.types, &sema);
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

/// Render diagnostics as `file:line:col: severity: message`, one per line.
pub fn render_diagnostics(path: &str, src: &str, diags: &[Diagnostic]) -> String {
    let mut out = String::new();
    for d in diags {
        let (line, col) = line_col(src, d.span.lo);
        let sev = match d.severity {
            sema::Severity::Error => "error",
            sema::Severity::Warning => "warning",
        };
        out.push_str(&format!("{path}:{line}:{col}: {sev}: {}\n", d.message));
    }
    out
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
BEGIN i := 0; s := {2, 3, 5, 7, 11, 13}; Walk(t, i, "hi")
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
        assert!(walk.ret.is_none());

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
  IMPORT Out := Texts;
  TYPE T = POINTER TO R; R = RECORD f: INTEGER END;
  VAR t: T; x: INTEGER;
  PROCEDURE g(a: INTEGER): INTEGER; BEGIN RETURN a END g;
BEGIN
  t := t(T);
  x := g(1);
  x := t(T).f;
  Out.Write(x);
  x := t.f;
  INC(x)
END M.
"#;

    #[test]
    fn reclassify_splits_guards_calls_and_module_qualification() {
        use crate::ast::Selector as Sel;
        use crate::resolve::{reclassify, Names};

        let mut m = parse_module(RECLASSIFY).expect("module must parse");
        let names = Names::collect(&m);
        reclassify(&mut m, &names).expect("reclassification must succeed");

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
    }

    #[test]
    fn reclassify_leaves_no_unresolved_selector_in_the_kitchen_sink() {
        use crate::ast::Selector as Sel;
        use crate::resolve::{reclassify, Names};

        let mut m = parse_module(KITCHEN_SINK).expect("kitchen sink must parse");
        let names = Names::collect(&m);
        reclassify(&mut m, &names).expect("reclassification must succeed");
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
        assert_eq!(calls, 6, "INC, FLT, ABS, ORD, ORD, Walk");
    }

    #[test]
    fn reclassify_rejects_a_case_label_that_is_neither_type_nor_module() {
        use crate::resolve::{reclassify, Names};

        let src = "MODULE M; VAR t: T; BEGIN CASE t OF NotAType: END END M.";
        let mut m = parse_module(src).expect("module must parse");
        let names = Names::collect(&m);
        let e = reclassify(&mut m, &names).expect_err("must be rejected");
        assert!(e.message.contains("NotAType"), "{}", e.message);

        // The same shape with a declared type is accepted.
        let src = "MODULE M; TYPE T = POINTER TO R; R = RECORD f: INTEGER END;
                   VAR t: T; BEGIN CASE t OF T: END END M.";
        let mut m = parse_module(src).expect("module must parse");
        let names = Names::collect(&m);
        reclassify(&mut m, &names).expect("`T` is a declared type");
    }
}

#[cfg(test)]
mod check_tests {
    use super::*;
    use crate::sym::Interface;

    fn iface(text: &str) -> Interface {
        sym::parse(text).unwrap_or_else(|e| panic!("fixture interface: {e}"))
    }

    /// The interfaces the report's own example modules need. `Texts.Writer` is
    /// a real record here, and `Oberon.Log` is typed through `Texts`, which is
    /// what makes the interface import list load-bearing.
    fn report_interfaces() -> Vec<Interface> {
        vec![
            iface("MODULE Texts\n\
TYPE Buffer = ARRAY 256 OF CHAR\n\
TYPE Writer = RECORD buf: Buffer END\n\
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
            .filter(Diagnostic::is_error)
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
            (46, "MODULE M; PROCEDURE F(): INTEGER; BEGIN END F; BEGIN END M.", "must end with a RETURN clause"),
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
            (51, "MODULE M; VAR i: INTEGER; BEGIN i := ABS END M.", "cannot be used as a value"),
            (52, "MODULE M; VAR s: SET; BEGIN s := {32} END M.", "outside 0..31"),
            (53, "MODULE M; IMPORT SYSTEM; BEGIN END M.", "module SYSTEM is not available"),
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
