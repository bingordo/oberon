//! Prototype of the resolver's *reclassification* pass.
//!
//! The grammar (see `oberon.lalrpop`) deliberately parses two constructs that
//! the Oberon-07 report describes differently but which have the same concrete
//! shape; this pass decides between them from the symbol table, before the type
//! checker ever runs (DESIGN.md §5, decisions D2 and D3):
//!
//! 1. `Selector::Args` is either a call's actual parameter list or a type guard.
//! 2. A designator's leading `.field` is either a record field select or a
//!    module qualifier.
//!
//! Both decisions are total, and neither needs the procedure's *signature*:
//!
//! * A paren group is a guard iff (a) it is followed by at least one more
//!   selector, or (b) it holds exactly one bare designator that denotes a type.
//!   Types and variables share one namespace in Oberon (report §4), so an
//!   identifier that denotes a type can never be the value argument of a call.
//! * The leading selector is a module qualifier iff the base identifier is a
//!   name bound by the module's import list.
//!
//! Only two name sets are needed: the type names visible at the designator and
//! the import aliases. A production resolver keeps those in the scope chain; the
//! prototype collects them once for the whole module, which is enough to prove
//! the rule (an inner declaration can only *shadow* a type name, never change
//! whether a given occurrence is one).

use crate::ast::*;
use std::collections::HashSet;

/// Names the prototype resolver needs. In the real resolver these live in the
/// scope chain built by `resolve.rs`; the shadowing case is the only difference.
#[derive(Debug, Default)]
pub struct Names {
    pub types: HashSet<String>,
    pub modules: HashSet<String>,
}

impl Names {
    /// The six predefined type names of report §6.1 plus the module's own
    /// imports and type declarations (recursively, including procedures).
    pub fn collect(m: &Module) -> Names {
        let mut n = Names::default();
        for t in ["BOOLEAN", "CHAR", "INTEGER", "REAL", "BYTE", "SET"] {
            n.types.insert(t.to_string());
        }
        for imp in &m.imports {
            n.modules.insert(imp.alias.clone());
        }
        n.collect_decls(&m.decls);
        n
    }

    fn collect_decls(&mut self, d: &Decls) {
        for t in &d.types {
            self.types.insert(t.name.clone());
        }
        for p in &d.procs {
            self.collect_decls(&p.decls);
        }
    }

    fn is_type(&self, q: &QIdent) -> bool {
        q.len() == 1 && self.types.contains(&q[0])
    }
    fn is_module(&self, name: &str) -> bool {
        self.modules.contains(name)
    }
}

#[derive(Debug, PartialEq)]
pub struct ReclassifyError {
    pub message: String,
    pub span: Span,
}

/// Rewrites `Selector::Args` into `Selector::Call` or `Selector::Guard`, and
/// folds a leading module qualifier into the designator's base.
pub fn reclassify(m: &mut Module, names: &Names) -> Result<(), ReclassifyError> {
    for s in &mut m.body {
        reclassify_stmt(s, names)?;
    }
    for p in &mut m.decls.procs {
        reclassify_proc(p, names)?;
    }
    for v in &mut m.decls.vars {
        reclassify_type_expr(&mut v.ty, names)?;
    }
    for t in &mut m.decls.types {
        reclassify_type_expr(&mut t.ty, names)?;
    }
    Ok(())
}

fn reclassify_proc(p: &mut Proc, names: &Names) -> Result<(), ReclassifyError> {
    for s in &mut p.body {
        reclassify_stmt(s, names)?;
    }
    if let Some(r) = &mut p.ret {
        reclassify_expr(r, names)?;
    }
    for c in &mut p.decls.consts {
        reclassify_expr(&mut c.value, names)?;
    }
    for v in &mut p.decls.vars {
        reclassify_type_expr(&mut v.ty, names)?;
    }
    for t in &mut p.decls.types {
        reclassify_type_expr(&mut t.ty, names)?;
    }
    for q in &mut p.decls.procs {
        reclassify_proc(q, names)?;
    }
    Ok(())
}

/// Array lengths and pointer/record base types are expressions/types too.
fn reclassify_type_expr(t: &mut TypeExpr, names: &Names) -> Result<(), ReclassifyError> {
    match t {
        TypeExpr::Array { dims, elem, .. } => {
            for d in dims {
                reclassify_expr(d, names)?;
            }
            reclassify_type_expr(elem, names)
        }
        TypeExpr::Record { fields, .. } => {
            for f in fields {
                reclassify_type_expr(&mut f.ty, names)?;
            }
            Ok(())
        }
        TypeExpr::Pointer(inner, _) => reclassify_type_expr(inner, names),
        TypeExpr::Named(..) | TypeExpr::Proc(..) => Ok(()),
    }
}

fn reclassify_stmt(s: &mut Stmt, names: &Names) -> Result<(), ReclassifyError> {
    match &mut s.kind {
        StmtKind::Assign { target, value } => {
            reclassify_designator(target, names)?;
            reclassify_expr(value, names)
        }
        StmtKind::Call(d) => reclassify_designator(d, names),
        StmtKind::If { arms, els } => {
            for (c, b) in arms {
                reclassify_expr(c, names)?;
                for s in b {
                    reclassify_stmt(s, names)?;
                }
            }
            if let Some(b) = els {
                for s in b {
                    reclassify_stmt(s, names)?;
                }
            }
            Ok(())
        }
        StmtKind::Case { subject, arms } => {
            reclassify_expr(subject, names)?;
            for a in arms {
                if let Some(labels) = &a.labels {
                    for l in labels {
                        for lab in std::iter::once(&l.lo).chain(l.hi.iter()) {
                            if let CaseLabel::Named(q) = lab {
                                if !names.is_type(q) && !names.is_module(&q[0]) {
                                    return Err(ReclassifyError {
                                        message: format!(
                                            "`{}` is neither a type nor a module: a record/pointer \
                                             CASE label must name a type (report 9.5)",
                                            q.join(".")
                                        ),
                                        span: a.span,
                                    });
                                }
                            }
                        }
                    }
                }
                for s in &mut a.body {
                    reclassify_stmt(s, names)?;
                }
            }
            Ok(())
        }
        StmtKind::While { arms } => {
            for (c, b) in arms {
                reclassify_expr(c, names)?;
                for s in b {
                    reclassify_stmt(s, names)?;
                }
            }
            Ok(())
        }
        StmtKind::Repeat { body, until } => {
            for s in body {
                reclassify_stmt(s, names)?;
            }
            reclassify_expr(until, names)
        }
        StmtKind::For {
            from, to, by, body, ..
        } => {
            reclassify_expr(from, names)?;
            reclassify_expr(to, names)?;
            if let Some(b) = by {
                reclassify_expr(b, names)?;
            }
            for s in body {
                reclassify_stmt(s, names)?;
            }
            Ok(())
        }
    }
}

fn reclassify_expr(e: &mut Expr, names: &Names) -> Result<(), ReclassifyError> {
    match &mut e.kind {
        ExprKind::Set(elems) => {
            for el in elems {
                reclassify_expr(&mut el.lo, names)?;
                if let Some(h) = &mut el.hi {
                    reclassify_expr(h, names)?;
                }
            }
            Ok(())
        }
        ExprKind::Designator(d) => reclassify_designator(d, names),
        ExprKind::Unary(_, x) => reclassify_expr(x, names),
        ExprKind::Binary(_, l, r) => {
            reclassify_expr(l, names)?;
            reclassify_expr(r, names)
        }
        ExprKind::TypeTest(x, _) => reclassify_expr(x, names),
        ExprKind::Int(_)
        | ExprKind::Real(_)
        | ExprKind::Char(_)
        | ExprKind::Str(_)
        | ExprKind::Bool(_)
        | ExprKind::Nil => Ok(()),
    }
}

fn reclassify_designator(d: &mut Designator, names: &Names) -> Result<(), ReclassifyError> {
    // D3: a leading `.field` after an import alias is module qualification.
    if d.base.len() == 1 {
        if let Some(Selector::Field(f)) = d.sels.first() {
            if names.is_module(&d.base[0]) {
                let name = f.clone();
                d.base.push(name);
                d.sels.remove(0);
            }
        }
    }
    // Nested designators (index expressions, arguments) are resolved first so
    // that the *arity* of the selector list is final before the split.
    for i in 0..d.sels.len() {
        let replacement = match std::mem::replace(&mut d.sels[i], Selector::Deref) {
            Selector::Index(mut idx) => {
                for e in &mut idx {
                    reclassify_expr(e, names)?;
                }
                Selector::Index(idx)
            }
            Selector::Args(Some(mut args)) => {
                for e in &mut args {
                    reclassify_expr(e, names)?;
                }
                Selector::Args(Some(args))
            }
            other => other,
        };
        d.sels[i] = replacement;
    }
    // D2: guard or call.
    for i in 0..d.sels.len() {
        let taken = std::mem::replace(&mut d.sels[i], Selector::Deref);
        d.sels[i] = match taken {
            Selector::Args(args) => match args.as_ref().and_then(|a| single_type_name(a, names)) {
                // `t(T)` is a guard whether or not more selectors follow: a
                // procedure call can never take a *type* as its only argument,
                // because types and variables share one namespace (report 4).
                Some(q) => Selector::Guard(q),
                None => Selector::Call(args.unwrap_or_default()),
            },
            other => other,
        };
    }
    Ok(())
}

/// `Some(q)` when the argument list is exactly one bare designator naming a type.
fn single_type_name(args: &[Expr], names: &Names) -> Option<QIdent> {
    if args.len() != 1 {
        return None;
    }
    match &args[0].kind {
        ExprKind::Designator(inner) if inner.sels.is_empty() && names.is_type(&inner.base) => {
            Some(inner.base.clone())
        }
        _ => None,
    }
}
