//! Small backend-independent, checked Oberon frontend boundary.
//!
//! This library does not know about any compiler's SSA, backends or machine IR.
//! It intentionally rejects constructs it cannot yet lower: no silent omissions.

use std::collections::HashMap;
use syntax::ast::{BinOp, Expr as OberonExpr, ExprKind, StmtKind, UnOp};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Module {
    pub name: String,
    pub functions: Vec<Function>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Function {
    pub name: String,
    pub parameters: Vec<String>,
    pub result: Expr,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expr {
    Parameter(usize),
    Integer(i32),
    Binary(Op, Box<Expr>, Box<Expr>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Add,
    Sub,
    Mul,
}

fn lower_expr(expr: &OberonExpr, params: &HashMap<&str, usize>) -> Result<Expr, String> {
    match &expr.kind {
        ExprKind::Int(value) => Ok(Expr::Integer(*value as i32)),
        ExprKind::Designator(d) if d.sels.is_empty() && d.base.len() == 1 => {
            params.get(d.base[0].as_str()).copied()
                .map(Expr::Parameter)
                .ok_or_else(|| format!("unsupported variable or constant: {}", d.base[0]))
        }
        ExprKind::Binary(op, a, b) => {
            let op = match op {
                BinOp::Add => Op::Add,
                BinOp::Sub => Op::Sub,
                BinOp::Mul => Op::Mul,
                _ => return Err(format!("unsupported binary expression: {op:?}")),
            };
            Ok(Expr::Binary(op, Box::new(lower_expr(a, params)?),
                            Box::new(lower_expr(b, params)?)))
        }
        ExprKind::Unary(UnOp::Plus, a) => lower_expr(a, params),
        ExprKind::Unary(UnOp::Neg, a) => Ok(Expr::Binary(
            Op::Sub, Box::new(Expr::Integer(0)), Box::new(lower_expr(a, params)?))),
        _ => Err(format!("unsupported expression at {}", expr.span.lo)),
    }
}

/// Parse and type-check through Oberon's existing frontend, then project a
/// deliberately small INTEGER-only procedure subset for an external compiler.
/// This is an in-process library call, not a separate frontend executable.
pub fn lower_module(source: &str) -> Result<Module, String> {
    let (checked, diagnostics) = sema::check_module_recovering(source, &[]);
    if let Some(d) = diagnostics.iter().find(|d| d.is_error()) {
        return Err(format!("Oberon semantic or syntax error: {d:?}"));
    }
    let module = checked.ok_or("Oberon parser did not produce a module")?;
    if !module.imports.is_empty() || !module.body.is_empty() ||
        !module.decls.vars.is_empty() || !module.decls.consts.is_empty() ||
        !module.decls.types.is_empty() {
        return Err("module imports, declarations and initialization are not yet supported".into());
    }
    let mut functions = Vec::new();
    for proc in &module.decls.procs {
        if !matches!(proc.result.as_deref(), Some([t]) if t == "INTEGER") {
            return Err(format!("{}: only INTEGER-returning functions are supported", proc.name));
        }
        if !proc.decls.procs.is_empty() || !proc.decls.vars.is_empty() ||
            !proc.decls.consts.is_empty() || !proc.decls.types.is_empty() {
            return Err(format!("{}: local declarations are not yet supported", proc.name));
        }
        if proc.body.len() != 1 {
            return Err(format!("{}: expected exactly one RETURN statement", proc.name));
        }
        let mut parameters = Vec::new();
        for section in &proc.params {
            if section.by_ref || section.ty.open != 0 || section.ty.base.len() != 1 ||
                section.ty.base[0] != "INTEGER" {
                return Err(format!("{}: only value INTEGER parameters are supported", proc.name));
            }
            for (name, _) in &section.names { parameters.push(name.clone()); }
        }
        let names: HashMap<_, _> = parameters.iter().enumerate()
            .map(|(i, s)| (s.as_str(), i)).collect();
        let result = match &proc.body[0].kind {
            StmtKind::Return(Some(expr)) => lower_expr(expr, &names)?,
            _ => return Err(format!("{}: expected RETURN expression", proc.name)),
        };
        functions.push(Function { name: proc.name.clone(), parameters, result });
    }
    if functions.is_empty() {
        return Err("no supported procedures in module".into());
    }
    Ok(Module { name: module.name, functions })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowers_checked_arithmetic_procedures() {
        let m = lower_module("MODULE Math; PROCEDURE Sum*(x, y: INTEGER): INTEGER; \
            BEGIN RETURN x + y * 3 END Sum; END Math.").unwrap();
        assert_eq!(m.name, "Math");
        assert_eq!(m.functions[0].parameters, ["x", "y"]);
        assert!(matches!(m.functions[0].result, Expr::Binary(Op::Add, _, _)));
    }

    #[test]
    fn type_errors_are_not_ignored() {
        let e = lower_module("MODULE M; PROCEDURE P*(x: INTEGER): INTEGER; \
            BEGIN RETURN unknown + x END P; END M.");
        assert!(e.is_err());
    }

    #[test]
    fn unsupported_control_flow_is_rejected() {
        let e = lower_module("MODULE M; PROCEDURE P*(x: INTEGER): INTEGER; \
            BEGIN IF x > 0 THEN RETURN x ELSE RETURN 0 END END P; END M.");
        assert!(e.is_err());
    }
}
