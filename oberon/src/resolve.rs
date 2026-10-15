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
//!   identifier that denotes a type can never be the value argument of a call —
//!   with one exception, `SYSTEM.SIZE(T)`, whose single argument *is* a type
//!   name by definition (report §11.1), so rule (b) is suppressed for that call.
//! * The leading selector is a module qualifier iff the base identifier is a
//!   name bound by the module's import list.
//!
//! Both decisions need the names visible *at that designator*: whether an
//! identifier denotes a type, and whether it is bound by the module's import
//! list. Those live in a scope chain here, not in one module-wide set, because an
//! inner declaration shadows an outer one — a local variable named `Texts` is not
//! the import alias, and a local `T` is not the type `T` (report §4; R17).

use crate::ast::*;
use std::collections::HashMap;

/// One scope's bindings, by kind: only the two questions the pass asks are
/// distinguished, but *every* declaration is recorded, because a variable or a
/// constant of some name shadows a type or an import alias of the same name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Type,
    Module,
    Other,
}

/// The scopes visible at the point being classified, innermost last. The
/// outermost scope holds the predefined types and the module's own imports and
/// declarations; a procedure body pushes its parameters and its own declarations.
#[derive(Debug, Default)]
pub struct Names {
    scopes: Vec<HashMap<String, Kind>>,
    /// The import aliases bound to report §11.1's builtin module `SYSTEM`.
    /// Every import is a `Kind::Module`, so the one paren group that must *not*
    /// follow rule (b) — `SYSTEM.SIZE(T)` — needs this extra set, and it is
    /// consulted through `lookup` so an inner declaration still shadows it (R17).
    system: Vec<String>,
}

impl Names {
    fn push(&mut self, scope: HashMap<String, Kind>) {
        self.scopes.push(scope);
    }
    fn pop(&mut self) {
        self.scopes.pop();
    }
    /// Bind a name in the scope currently open — a procedure's own name, which
    /// its later siblings and the enclosing body can see.
    fn declare_here(&mut self, name: &str) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string(), Kind::Other);
        }
    }
    /// The innermost binding of `name`, so an inner declaration shadows an outer.
    fn lookup(&self, name: &str) -> Option<Kind> {
        self.scopes
            .iter()
            .rev()
            .find_map(|s| s.get(name).copied())
    }
}

impl Names {
    /// The outermost scope: the predefined types of report §6.1, the module's
    /// imports, and everything the module declares. Inner scopes are pushed by
    /// the walk.
    pub fn collect(m: &Module) -> Names {
        let mut n = Names::default();
        let mut scope: HashMap<String, Kind> = HashMap::new();
        for t in ["BOOLEAN", "CHAR", "INTEGER", "REAL", "BYTE", "SET"] {
            scope.insert(t.to_string(), Kind::Type);
        }
        for imp in &m.imports {
            scope.insert(imp.alias.clone(), Kind::Module);
            if imp.module == "SYSTEM" {
                n.system.push(imp.alias.clone());
            }
        }
        decl_bindings(&m.decls, &mut scope);
        n.push(scope);
        n
    }

    fn is_type(&self, q: &QIdent) -> bool {
        q.len() == 1 && self.lookup(&q[0]) == Some(Kind::Type)
    }
    fn is_module(&self, name: &str) -> bool {
        self.lookup(name) == Some(Kind::Module)
    }
    /// Whether `name` is an alias the import list bound to `SYSTEM` (report
    /// §11.1) *and* is still the module in the scope the designator sits in.
    fn is_system(&self, name: &str) -> bool {
        self.is_module(name) && self.system.iter().any(|s| s == name)
    }
}

#[derive(Debug, PartialEq)]
pub struct ReclassifyError {
    pub message: String,
    pub span: Span,
}

/// The names a declaration sequence binds in the scope it opens. A nested
/// procedure's declarations belong to *its* scope, not to this one.
fn decl_bindings(d: &Decls, scope: &mut HashMap<String, Kind>) {
    for c in &d.consts {
        scope.insert(c.name.clone(), Kind::Other);
    }
    for t in &d.types {
        scope.insert(t.name.clone(), Kind::Type);
    }
    for v in &d.vars {
        for (name, _) in &v.names {
            scope.insert(name.clone(), Kind::Other);
        }
    }
}

/// Rewrites `Selector::Args` into `Selector::Call` or `Selector::Guard`, and
/// folds a leading module qualifier into the designator's base.
pub fn reclassify(m: &mut Module, names: &mut Names) -> Result<(), ReclassifyError> {
    // Declarations first, in order, each procedure's name entering the module's
    // scope as its declaration is reached (report §4): a procedure is then
    // visible to the ones declared after it and to the module body, and not to
    // the ones before it.
    for p in &mut m.decls.procs {
        names.push(entry(&p.name));
        reclassify_proc(p, names)?;
    }
    for v in &mut m.decls.vars {
        reclassify_type_expr(&mut v.ty, names)?;
    }
    for t in &mut m.decls.types {
        reclassify_type_expr(&mut t.ty, names)?;
    }
    // The body can now see every procedure.
    for s in &mut m.body {
        reclassify_stmt(s, names)?;
    }
    Ok(())
}

/// One binding, for a name that is not a type and not a module alias.
fn entry(name: &str) -> HashMap<String, Kind> {
    std::iter::once((name.to_string(), Kind::Other)).collect()
}

fn reclassify_proc(p: &mut Proc, names: &mut Names) -> Result<(), ReclassifyError> {
    // A procedure's own scope: its parameters first, then its declarations —
    // either can shadow an outer type name or import alias.
    let mut scope: HashMap<String, Kind> = HashMap::new();
    for sec in &p.params {
        for (name, _) in &sec.names {
            scope.insert(name.clone(), Kind::Other);
        }
    }
    decl_bindings(&p.decls, &mut scope);
    names.push(scope);
    // Nested declarations first, and each nested procedure's name enters *this*
    // scope as its declaration is reached, so its siblings after it see it while
    // the ones before it do not (report §4).
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
        let qname = q.name.clone();
        names.declare_here(&qname);
        reclassify_proc(q, names)?;
    }
    // The body last: by now it can see every declaration of this scope, including
    // the nested procedures.
    for s in &mut p.body {
        reclassify_stmt(s, names)?;
    }
    if let Some(r) = &mut p.ret {
        reclassify_expr(r, names)?;
    }
    names.pop();
    Ok(())
}

/// Array lengths and pointer/record base types are expressions/types too.
fn reclassify_type_expr(t: &mut TypeExpr, names: &mut Names) -> Result<(), ReclassifyError> {
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

fn reclassify_stmt(s: &mut Stmt, names: &mut Names) -> Result<(), ReclassifyError> {
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

fn reclassify_expr(e: &mut Expr, names: &mut Names) -> Result<(), ReclassifyError> {
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

fn reclassify_designator(d: &mut Designator, names: &mut Names) -> Result<(), ReclassifyError> {
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
            Selector::Args(args) => {
                // Two calls take a *type* as their single argument —
                // `SYSTEM.SIZE(T)` (report §11.1) and `MIN(T)`/`MAX(T)` (W8.2) —
                // so rule (b) must not swallow them: the paren group stays a
                // call. A guard, after all, needs a designator of a record or
                // pointer type, which none of these names can be.
                let takes_a_type = matches!(d.base.as_slice(), [alias, entry]
                    if entry == "SIZE" && names.is_system(alias))
                    || matches!(d.base.as_slice(), [n] if n == "MIN" || n == "MAX");
                match args.as_ref().and_then(|a| single_type_name(a, names)) {
                    // `t(T)` is a guard whether or not more selectors follow: a
                    // procedure call can never take a *type* as its only
                    // argument, because types and variables share one namespace
                    // (report 4).
                    Some(q) if !takes_a_type => Selector::Guard(q),
                    _ => Selector::Call(args.unwrap_or_default()),
                }
            }
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
