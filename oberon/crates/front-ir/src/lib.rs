//! Backend-independent projection of checked Oberon procedures.
//!
//! Only syntax and semantic-analysis crates are dependencies. Unsupported
//! constructs fail closed; the consuming compiler owns SSA and code generation.
use std::collections::HashMap;
use syntax::ast::{BinOp, Expr as OberonExpr, ExprKind, Stmt as OberonStmt,
                  StmtKind, TypeExpr, UnOp};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Module {
    pub name: String,
    pub functions: Vec<Function>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Function {
    pub name: String,
    /// Value INTEGER parameters, in source order.
    pub parameters: Vec<String>,
    /// Zero-initialized INTEGER locals, in source order.
    pub locals: Vec<String>,
    /// Fixed-size single-dimensional local INTEGER arrays. Nonzero lengths.
    pub arrays: Vec<ArrayLocal>,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArrayLocal {
    pub name: String,
    pub length: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expr {
    Variable(usize),
    /// Index into Function.arrays; the subscript is a checked INTEGER expr.
    ArrayElement { array: usize, index: Box<Expr> },
    Integer(i32),
    Boolean(bool),
    Binary(Op, Box<Expr>, Box<Expr>),
    /// Checked same-module INTEGER function call, including recursion.
    Call { name:String, args:Vec<Expr> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Add, Sub, Mul,
    Eq, Ne, Lt, Le, Gt, Ge,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stmt {
    Assign { variable: usize, value: Expr },
    AssignArray { array: usize, index: Expr, value: Expr },
    Return(Expr),
    /// Evaluate a procedure call for its effect, ignoring its result.
    Call(Expr),
    If { condition: Expr, then_body: Vec<Stmt>, else_body: Vec<Stmt> },
    While { condition: Expr, body: Vec<Stmt> },
}

fn lower_call(d:&syntax::ast::Designator,
              names:&HashMap<&str,usize>,
              arrays:&HashMap<&str,usize>,
              functions:&HashMap<String,usize>)->Result<Expr,String> {
    if d.base.len()!=1 || d.sels.len()!=1 {
        return Err("unsupported qualified or selected function call".into());
    }
    let symbol=&d.base[0];
    let expected=*functions.get(symbol)
        .ok_or_else(||format!("unsupported call to non-local function {symbol}"))?;
    let raw_args=match &d.sels[0] {
        syntax::ast::Selector::Call(args)=>args.as_slice(),
        syntax::ast::Selector::Args(args)=>args.as_deref().unwrap_or(&[]),
        _=>return Err("expected direct function call".into()),
    };
    if raw_args.len()!=expected {
        return Err(format!("{symbol}: call argument count mismatch"));
    }
    let mut args=Vec::with_capacity(raw_args.len());
    for arg in raw_args {
        args.push(lower_expr(arg,names,arrays,functions)?);
    }
    Ok(Expr::Call{name:symbol.clone(),args})
}

fn lower_expr(expr: &OberonExpr, names: &HashMap<&str, usize>,
              arrays: &HashMap<&str, usize>,
              functions:&HashMap<String,usize>) -> Result<Expr, String> {
    match &expr.kind {
        ExprKind::Designator(d) if d.base.len()==1 && d.sels.len()==1
            && matches!(d.sels[0],syntax::ast::Selector::Call(_)|
                        syntax::ast::Selector::Args(_)) => {
            lower_call(d,names,arrays,functions)
        }
        ExprKind::Int(value) => Ok(Expr::Integer(*value as i32)),
        ExprKind::Bool(value) => Ok(Expr::Boolean(*value)),
        ExprKind::Designator(d) if d.sels.is_empty() && d.base.len() == 1 => {
            names.get(d.base[0].as_str()).copied().map(Expr::Variable)
                .ok_or_else(|| format!("unsupported variable or constant: {}", d.base[0]))
        }
        ExprKind::Designator(d) if d.base.len()==1 && d.sels.len()==1 => {
            match &d.sels[0] {
                syntax::ast::Selector::Index(indices) if indices.len()==1 => {
                    let array=*arrays.get(d.base[0].as_str())
                        .ok_or_else(||format!("not a supported local array: {}",d.base[0]))?;
                    Ok(Expr::ArrayElement {
                        array,index:Box::new(lower_expr(&indices[0],names, arrays, functions)?),
                    })
                }
                _=>Err(format!("unsupported array selector at {}",d.span.lo)),
            }
        },
        ExprKind::Binary(op, a, b) => {
            let op = match op {
                BinOp::Add => Op::Add,
                BinOp::Sub => Op::Sub,
                BinOp::Mul => Op::Mul,
                BinOp::Eq => Op::Eq,
                BinOp::Ne => Op::Ne,
                BinOp::Lt => Op::Lt,
                BinOp::Le => Op::Le,
                BinOp::Gt => Op::Gt,
                BinOp::Ge => Op::Ge,
                _ => return Err(format!("unsupported binary expression: {op:?}")),
            };
            Ok(Expr::Binary(op, Box::new(lower_expr(a, names, arrays, functions)?),
                            Box::new(lower_expr(b, names, arrays, functions)?)))
        }
        ExprKind::Unary(UnOp::Plus, a) => lower_expr(a, names, arrays, functions),
        ExprKind::Unary(UnOp::Neg, a) =>
            Ok(Expr::Binary(Op::Sub, Box::new(Expr::Integer(0)),
                            Box::new(lower_expr(a, names, arrays, functions)?))),
        _ => Err(format!("unsupported expression at {}", expr.span.lo)),
    }
}

fn lower_stmts(stmts: &[OberonStmt], names: &HashMap<&str, usize>,
              arrays: &HashMap<&str, usize>,
              functions:&HashMap<String,usize>) -> Result<Vec<Stmt>, String>
{
    let mut out = Vec::new();
    for s in stmts {
        match &s.kind {
            StmtKind::Call(d)=>out.push(Stmt::Call(
                lower_call(d,names,arrays,functions)?)),
            StmtKind::Assign { target, value } if target.sels.is_empty() &&
                                                   target.base.len() == 1 => {
                let variable = names.get(target.base[0].as_str()).copied()
                    .ok_or_else(|| format!("unknown local or parameter: {}", target.base[0]))?;
                out.push(Stmt::Assign { variable, value: lower_expr(value, names, arrays, functions)? });
            }
            StmtKind::Assign { target, value } if target.base.len()==1 &&
                                                    target.sels.len()==1 => {
                match &target.sels[0] {
                    syntax::ast::Selector::Index(indices) if indices.len()==1 => {
                        let array=*arrays.get(target.base[0].as_str())
                            .ok_or_else(||format!("not a supported local array: {}",target.base[0]))?;
                        out.push(Stmt::AssignArray {
                            array,
                            index:lower_expr(&indices[0],names, arrays, functions)?,
                            value:lower_expr(value,names, arrays, functions)?,
                        });
                    }
                    _=>return Err(format!("unsupported array assignment at {}",s.span.lo)),
                }
            }
            StmtKind::Return(Some(e)) =>
                out.push(Stmt::Return(lower_expr(e, names, arrays, functions)?)),
            StmtKind::If { arms, els } => {
                if arms.is_empty() { return Err("IF with no arm".into()); }
                let mut tail = match els {
                    Some(stmts) => lower_stmts(stmts, names, arrays, functions)?,
                    None => Vec::new(),
                };
                // ELSIF is lowered into a nested IF in the previous else arm.
                for (condition, body) in arms.iter().rev() {
                    tail = vec![Stmt::If {
                        condition: lower_expr(condition, names, arrays, functions)?,
                        then_body: lower_stmts(body, names, arrays, functions)?,
                        else_body: tail,
                    }];
                }
                out.extend(tail);
            }
            StmtKind::While { arms } if arms.len() == 1 => {
                let (condition, body) = &arms[0];
                out.push(Stmt::While {
                    condition: lower_expr(condition, names, arrays, functions)?,
                    body: lower_stmts(body, names, arrays, functions)?,
                });
            }
            _ => return Err(format!("unsupported Oberon statement at {}", s.span.lo)),
        }
    }
    Ok(out)
}

/// Parse/type-check in process, then export a small checked procedure subset.
/// Module imports and external/runtime references are not implemented.
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

    // Predeclare direct-call signatures before lowering bodies, so forward
    // references and recursive calls resolve without introducing imports.
    let mut signatures=HashMap::<String,usize>::new();
    for proc in &module.decls.procs {
        let count=proc.params.iter().map(|section|section.names.len()).sum();
        if signatures.insert(proc.name.clone(),count).is_some() {
            return Err(format!("duplicate procedure {}",proc.name));
        }
    }
    let mut functions = Vec::new();
    for proc in &module.decls.procs {
        if !matches!(proc.result.as_deref(), Some([t]) if t == "INTEGER") {
            return Err(format!("{}: only INTEGER-returning functions are supported", proc.name));
        }
        if !proc.decls.procs.is_empty() || !proc.decls.consts.is_empty() ||
           !proc.decls.types.is_empty() {
            return Err(format!("{}: nested procedures, constants or types unsupported", proc.name));
        }
        let mut parameters = Vec::new();
        for section in &proc.params {
            if section.by_ref || section.ty.open != 0 ||
               section.ty.base.as_slice() != ["INTEGER"] {
                return Err(format!("{}: only value INTEGER parameters are supported", proc.name));
            }
            for (name, _) in &section.names { parameters.push(name.clone()); }
        }
        let mut locals = Vec::new();
        let mut fixed_arrays = Vec::new();
        for decl in &proc.decls.vars {
            match &decl.ty {
                TypeExpr::Named(q, _) if q.as_slice()==["INTEGER"] => {
                    for (name,_) in &decl.names {locals.push(name.clone());}
                }
                TypeExpr::Array{dims,elem,..} if dims.len()==1 &&
                    matches!(elem.as_ref(),TypeExpr::Named(q,_) if q.as_slice()==["INTEGER"]) => {
                    // Do not invent a constant evaluator or assume a stride
                    // for a non-integer/multidimensional element type.
                    let ExprKind::Int(length)=&dims[0].kind else {
                        return Err(format!("{}: array length must be an INTEGER literal",proc.name));
                    };
                    if *length==0 || *length>1024 {
                        return Err(format!("{}: array length must be within 1..=1024",proc.name));
                    }
                    for (name,_) in &decl.names {
                        fixed_arrays.push(ArrayLocal{name:name.clone(),length:*length});
                    }
                }
                _=>return Err(format!("{}: only INTEGER locals and fixed INTEGER arrays supported",proc.name)),
            }
        }
        let names: HashMap<_, _> = parameters.iter().chain(locals.iter())
            .enumerate().map(|(i, name)| (name.as_str(), i)).collect();
        let arrays:HashMap<_,_>=fixed_arrays.iter().enumerate()
            .map(|(i,a)|(a.name.as_str(),i)).collect();
        let body = lower_stmts(&proc.body, &names, &arrays, &signatures)?;
        // An absent return is rejected rather than relying on Oberon's
        // zero-valued return accumulator (not modeled by this narrow bridge).
        if !contains_return(&body) {
            return Err(format!("{}: function must return a value", proc.name));
        }
        functions.push(Function { name: proc.name.clone(), parameters, locals,
                                  arrays:fixed_arrays, body });
    }
    if functions.is_empty() { return Err("no supported procedures in module".into()); }
    Ok(Module { name: module.name, functions })
}

fn contains_return(body: &[Stmt]) -> bool {
    body.iter().any(|s| match s {
        Stmt::Return(_) => true,
        Stmt::If { then_body, else_body, .. } =>
            contains_return(then_body) || contains_return(else_body),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowers_checked_integer_calls_and_rejects_bad_signatures() {
        let src="MODULE Calls;
            PROCEDURE Twice*(n: INTEGER): INTEGER;
            BEGIN RETURN Add(n,n) END Twice;
            PROCEDURE Add*(a,b: INTEGER): INTEGER;
            BEGIN RETURN a+b END Add;
        END Calls.";
        let module=lower_module(src).unwrap();
        assert_eq!(module.functions.len(),2);
        assert!(matches!(&module.functions[0].body[0],
            Stmt::Return(Expr::Call{name,args}) if
                name=="Add" && args.len()==2));
        let bad="MODULE Calls;
            PROCEDURE Add*(a,b: INTEGER): INTEGER;
            BEGIN RETURN a+b END Add;
            PROCEDURE Wrong*(a: INTEGER): INTEGER;
            BEGIN RETURN Add(a) END Wrong;
        END Calls.";
        assert!(lower_module(bad).is_err());
        let qualified="MODULE Calls; IMPORT Out;
            PROCEDURE Wrong*(a: INTEGER): INTEGER;
            BEGIN Out.Int(a); RETURN a END Wrong;
        END Calls.";
        assert!(lower_module(qualified).is_err());
    }

    #[test]
    fn lowers_checked_arithmetic_procedures() {
        let m = lower_module("MODULE Math; PROCEDURE Sum*(x, y: INTEGER): INTEGER; \
            BEGIN RETURN x + y * 3 END Sum; END Math.").unwrap();
        assert_eq!(m.name, "Math");
        assert_eq!(m.functions[0].parameters, ["x", "y"]);
        assert!(matches!(m.functions[0].body[0], Stmt::Return(Expr::Binary(Op::Add, _, _))));
    }

    #[test]
    fn lowers_integer_locals_if_and_while() {
        let m = lower_module("MODULE M; PROCEDURE Count*(n: INTEGER): INTEGER; \
            VAR result: INTEGER; BEGIN result := 0; \
            WHILE n > 0 DO IF n > 2 THEN result := result + 2 \
            ELSE result := result + 1 END; n := n - 1 END; \
            RETURN result END Count; END M.").unwrap();
        let p = &m.functions[0];
        assert_eq!(p.parameters, ["n"]);
        assert_eq!(p.locals, ["result"]);
        assert!(matches!(p.body[1], Stmt::While { .. }));
    }

    #[test]
    fn lowers_checked_fixed_arrays_and_indexed_expressions() {
        let src="MODULE Arrays; PROCEDURE Sum*(n: INTEGER): INTEGER;
            VAR arr: ARRAY 4 OF INTEGER; other: INTEGER;
            BEGIN arr[0] := n; arr[1] := arr[0] + 2;
            other := arr[1]; RETURN other END Sum; END Arrays.";
        let m=lower_module(src).unwrap();
        let p=&m.functions[0];
        assert_eq!(p.arrays,vec![ArrayLocal{name:"arr".into(),length:4}]);
        assert_eq!(p.locals,vec!["other"]);
        assert!(matches!(&p.body[0],Stmt::AssignArray{array:0,..}));
        assert!(matches!(&p.body[1],Stmt::AssignArray{
            value:Expr::Binary(Op::Add,_,_),..}));
        let e=lower_module("MODULE A; PROCEDURE T*(x: INTEGER): INTEGER;
            VAR a: ARRAY 0 OF INTEGER;
            BEGIN RETURN x END T; END A.");
        assert!(e.is_err(),"zero-length arrays must not be silently accepted");
        let e=lower_module("MODULE A; PROCEDURE T*(x: INTEGER): INTEGER;
            VAR a: ARRAY 2, 3 OF INTEGER;
            BEGIN RETURN x END T; END A.");
        assert!(e.is_err(),"multidimensional arrays need separate layout rules");
    }

    #[test]
    fn type_errors_are_not_ignored() {
        let e = lower_module("MODULE M; PROCEDURE P*(x: INTEGER): INTEGER; \
            BEGIN RETURN unknown + x END P; END M.");
        assert!(e.is_err());
    }

    #[test]
    fn unsupported_operations_fail_closed() {
        let e = lower_module("MODULE M; PROCEDURE P*(x: INTEGER): INTEGER; \
            BEGIN RETURN x DIV 3 END P; END M.");
        assert!(e.unwrap_err().contains("unsupported binary"));
        let e = lower_module("MODULE M; PROCEDURE P*(x: INTEGER): INTEGER; \
            VAR z: REAL; BEGIN RETURN x END P; END M.");
        assert!(e.unwrap_err().contains("INTEGER local"));
    }
}
