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
    /// Module-level INTEGER objects, in source order.
    pub globals: Vec<Global>,
    pub global_arrays: Vec<ArrayGlobal>,
    pub functions: Vec<Function>,
    /// Checked module BEGIN body; emitted as a private constructor.
    pub initializer: Option<Function>,
}

/// Exported module objects are linker-visible; private objects keep local
/// linkage. The bridge currently supports zero-initialized INTEGER only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Global {
    pub name: String,
    pub exported: bool,
}

/// Zero-initialized, bounds-checked module INTEGER array.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArrayGlobal {
    pub name: String,
    pub length: u32,
    pub exported: bool,
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
    /// Module arrays follow locals in array-index numbering.
    pub global_arrays: Vec<ArrayGlobal>,
    /// Ordered module globals. Variable indexes after parameters and
    /// local scalar variables refer to this list (never to frame slots).
    pub globals: Vec<String>,
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
/// Same-module integer calls to declared functions and recursive calls are
/// projected only when the callee signature is supported (up to sixteen value
/// INTEGER parameters). Module imports and external/runtime references are
/// not implemented.
pub fn lower_module(source: &str) -> Result<Module, String> {
    let (checked, diagnostics) = sema::check_module_recovering(source, &[]);
    if let Some(d) = diagnostics.iter().find(|d| d.is_error()) {
        return Err(format!("Oberon semantic or syntax error: {d:?}"));
    }
    let module = checked.ok_or("Oberon parser did not produce a module")?;
    if !module.imports.is_empty() ||
       !module.decls.consts.is_empty() || !module.decls.types.is_empty() {
        return Err("module imports, constants or types are not yet supported".into());
    }
    let mut globals = Vec::<Global>::new();
    let mut global_arrays = Vec::<ArrayGlobal>::new();
    for decl in &module.decls.vars {
        match &decl.ty {
            TypeExpr::Named(q, _) if q.as_slice()==["INTEGER"] => {
                for (name, exported) in &decl.names {
                    globals.push(Global { name: name.clone(), exported: *exported });
                }
            }
            TypeExpr::Array { dims, elem, .. } if dims.len()==1 &&
                matches!(elem.as_ref(),TypeExpr::Named(q,_) if q.as_slice()==["INTEGER"]) => {
                let ExprKind::Int(length)=&dims[0].kind else {
                    return Err("module array length must be an INTEGER literal".into());
                };
                if *length == 0 || *length > 1024 {
                    return Err("module array length must be within 1..=1024".into());
                }
                for (name, exported) in &decl.names {
                    global_arrays.push(ArrayGlobal {
                        name: name.clone(), length: *length, exported: *exported,
                    });
                }
            }
            _ => return Err("only INTEGER globals and fixed INTEGER arrays are supported".into()),
        }
    }

    // Collect the checked signatures before lowering procedure bodies. The
    // Oberon semantic checker already enforces source declaration order;
    // this map does not authorize forward references.
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
        // Modules and procedures use separate scopes: a local or parameter
        // shadows a global of the same name. Global indexes begin after
        // the complete local scalar index range.
        let mut names: HashMap<&str, usize> = globals.iter().enumerate()
            .map(|(i, g)| (g.name.as_str(), parameters.len() + locals.len() + i))
            .collect();
        for (i, name) in parameters.iter().chain(locals.iter()).enumerate() {
            names.insert(name.as_str(), i);
        }
        // Local array names shadow module array names; global arrays
        // follow local arrays in the checked bridge's array index space.
        let mut arrays:HashMap<&str,usize>=global_arrays.iter().enumerate()
            .map(|(i,a)|(a.name.as_str(),fixed_arrays.len()+i)).collect();
        for (i, array) in fixed_arrays.iter().enumerate() {
            arrays.insert(array.name.as_str(), i);
        }
        let body = lower_stmts(&proc.body, &names, &arrays, &signatures)?;
        // An absent return is rejected rather than relying on Oberon's
        // zero-valued return accumulator (not modeled by this narrow bridge).
        if !contains_return(&body) {
            return Err(format!("{}: function must return a value", proc.name));
        }
        functions.push(Function { name: proc.name.clone(), parameters, locals,
                                  arrays:fixed_arrays,
                                  global_arrays:global_arrays.clone(),
                                  globals: globals.iter().map(|g| g.name.clone()).collect(),
                                  body });
    }
    let initializer = if module.body.is_empty() {
        None
    } else {
        // Module code has no parameters or function-local variables. Only
        // verified module globals are legal roots for assignments/indexing.
        let names: HashMap<&str, usize> = globals.iter().enumerate()
            .map(|(i, item)| (item.name.as_str(), i)).collect();
        let arrays: HashMap<&str, usize> = global_arrays.iter().enumerate()
            .map(|(i, item)| (item.name.as_str(), i)).collect();
        let body = lower_stmts(&module.body, &names, &arrays, &signatures)?;
        if contains_return(&body) {
            return Err("module initialization cannot return a value".into());
        }
        Some(Function {
            name: "__module_init".into(),
            parameters: Vec::new(), locals: Vec::new(), arrays: Vec::new(),
            global_arrays: global_arrays.clone(),
            globals: globals.iter().map(|g| g.name.clone()).collect(),
            body,
        })
    };
    if functions.is_empty() && globals.is_empty() && global_arrays.is_empty()
        && initializer.is_none() {
        return Err("module has no supported procedures, globals or initialization".into());
    }
    Ok(Module { name: module.name, globals, global_arrays, functions, initializer })
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
    fn projects_checked_module_begin_using_globals_arrays_and_calls() {
        let source="MODULE Init;
            VAR counter*: INTEGER; data*: ARRAY 4 OF INTEGER;
            PROCEDURE Twice*(n: INTEGER): INTEGER;
            BEGIN RETURN n * 2 END Twice;
            BEGIN
                counter := Twice(11);
                data[2] := counter + 3;
                IF data[2] > 0 THEN counter := data[2] END
            END Init.";
        let module=lower_module(source).unwrap();
        let init=module.initializer.as_ref().unwrap();
        assert_eq!(init.name,"__module_init");
        assert!(init.parameters.is_empty()&&init.locals.is_empty()&&init.arrays.is_empty());
        assert_eq!(init.globals,["counter"]);
        assert_eq!(init.global_arrays[0].length,4);
        assert!(matches!(&init.body[0],Stmt::Assign{variable:0,
            value:Expr::Call{name,args}} if name=="Twice"&&args.len()==1));
        assert!(matches!(&init.body[1],Stmt::AssignArray{array:0,..}));
        assert!(matches!(&init.body[2],Stmt::If{..}));
        assert!(module.functions.iter().all(|f|f.name!="__module_init"));
    }

    #[test]
    fn bodyless_and_body_only_modules_remain_distinct() {
        let old=lower_module("MODULE M; VAR n*: INTEGER; END M.").unwrap();
        assert!(old.initializer.is_none());
        let source="MODULE M;
            VAR n*: INTEGER;
            BEGIN n := 47
            END M.";
        let init=lower_module(source).unwrap();
        assert!(matches!(init.initializer.unwrap().body.as_slice(),
            [Stmt::Assign{variable:0,value:Expr::Integer(47)}]));
        let invalid="MODULE M; VAR n*: INTEGER;
            BEGIN n := TRUE END M.";
        assert!(lower_module(invalid).is_err());
    }

    #[test]
    fn lowers_checked_integer_calls_and_rejects_bad_signatures() {
        let src="MODULE Calls;
            PROCEDURE Add*(a,b: INTEGER): INTEGER;
            BEGIN RETURN a+b END Add;
            PROCEDURE Twice*(n: INTEGER): INTEGER;
            BEGIN RETURN Add(n,n) END Twice;
        END Calls.";
        let module=lower_module(src).unwrap();
        assert_eq!(module.functions.len(),2);
        assert!(matches!(&module.functions[1].body[0],
            Stmt::Return(Expr::Call{name,args}) if
                name=="Add" && args.len()==2));
        // The semantic checker follows Oberon's declaration-order rule:
        // calling a later procedure without a prior declaration is invalid.
        let forward="MODULE Calls;
            PROCEDURE Twice*(n: INTEGER): INTEGER;
            BEGIN RETURN Add(n,n) END Twice;
            PROCEDURE Add*(a,b: INTEGER): INTEGER;
            BEGIN RETURN a+b END Add;
        END Calls.";
        assert!(lower_module(forward).is_err());
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
    fn module_integer_globals_and_local_shadowing() {
        let source="MODULE Globals;
            VAR counter*, private: INTEGER;
            PROCEDURE Add*(step: INTEGER): INTEGER;
            BEGIN counter := counter + step; RETURN counter END Add;
            PROCEDURE Shadow*(step: INTEGER): INTEGER;
            VAR counter: INTEGER;
            BEGIN counter := step + 7; RETURN counter END Shadow;
            PROCEDURE Private*(step: INTEGER): INTEGER;
            BEGIN private := private + step; RETURN private END Private;
            END Globals.";
        let module=lower_module(source).unwrap();
        assert_eq!(module.globals, vec![
            Global{name:"counter".into(),exported:true},
            Global{name:"private".into(),exported:false},
        ]);
        assert_eq!(module.functions.len(), 3);
        assert_eq!(module.functions[0].globals, ["counter", "private"]);
        let add=&module.functions[0];
        // One value parameter, no locals: global counter is index 1.
        assert!(matches!(&add.body[0], Stmt::Assign{variable:1,..}));
        let shadow=&module.functions[1];
        // One param, one local: local counter is index 1, module
        // counter is index 2 and must NOT be used by this procedure.
        assert!(matches!(&shadow.body[0],Stmt::Assign{variable:1,..}));
        let private=&module.functions[2];
        assert!(matches!(&private.body[0],Stmt::Assign{variable:2,..}));
    }

    #[test]
    fn module_with_only_global_storage_is_a_valid_data_translation_unit() {
        let m=lower_module("MODULE Data; VAR counter*, hidden: INTEGER; END Data.").unwrap();
        assert!(m.functions.is_empty());
        assert_eq!(m.globals,vec![
            Global{name:"counter".into(),exported:true},
            Global{name:"hidden".into(),exported:false},
        ]);
    }

    #[test]
    fn checked_module_arrays_support_reads_stores_and_local_shadowing() {
        let src="MODULE Arrays;
            VAR shared*: ARRAY 4 OF INTEGER; hidden: ARRAY 3 OF INTEGER;
            PROCEDURE Update*(n: INTEGER): INTEGER;
            BEGIN shared[1] := n; hidden[2] := shared[1] + 2;
            RETURN hidden[2] END Update;
            PROCEDURE Shadow*(n: INTEGER): INTEGER;
            VAR shared: ARRAY 2 OF INTEGER;
            BEGIN shared[0] := n; RETURN shared[0] END Shadow;
            END Arrays.";
        let module=lower_module(src).unwrap();
        assert_eq!(module.global_arrays, vec![
            ArrayGlobal{name:"shared".into(),length:4,exported:true},
            ArrayGlobal{name:"hidden".into(),length:3,exported:false},
        ]);
        assert!(module.globals.is_empty());
        let update=&module.functions[0];
        assert!(update.arrays.is_empty());
        assert!(matches!(&update.body[0],Stmt::AssignArray{array:0,..}));
        assert!(matches!(&update.body[1],Stmt::AssignArray{array:1,..}));
        let shadow=&module.functions[1];
        assert_eq!(shadow.arrays.len(),1);
        assert!(matches!(&shadow.body[0],Stmt::AssignArray{array:0,..}));
        assert_eq!(shadow.global_arrays.len(),2);
    }

    #[test]
    fn data_only_array_module_is_accepted_but_unsupported_extents_are_rejected() {
        let module=lower_module("MODULE Data; VAR values*: ARRAY 8 OF INTEGER; END Data.").unwrap();
        assert!(module.functions.is_empty());
        assert_eq!(module.global_arrays[0].length,8);
        for dim in ["0", "1025", "2, 2"] {
            let src=format!("MODULE Data; VAR values: ARRAY {dim} OF INTEGER; END Data.");
            assert!(lower_module(&src).is_err());
        }
    }

    #[test]
    fn unsupported_global_types_are_rejected() {
        let array="MODULE M; VAR a: ARRAY 3, 2 OF INTEGER;
            PROCEDURE R*(v: INTEGER): INTEGER;
            BEGIN RETURN v END R; END M.";
        assert!(lower_module(array).unwrap_err().contains("fixed INTEGER arrays"));
        let real="MODULE M; VAR a: REAL;
            PROCEDURE R*(v: INTEGER): INTEGER;
            BEGIN RETURN v END R; END M.";
        assert!(lower_module(real).is_err());
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
