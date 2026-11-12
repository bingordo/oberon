use oberon_front_ir::{lower_module,Expr,Stmt};
const SOURCE:&str=r#"
MODULE Ref;
VAR g: INTEGER;
PROCEDURE Bump*(VAR x: INTEGER; amount: INTEGER): INTEGER;
BEGIN x:=x+amount; RETURN x END Bump;
PROCEDURE Forward*(VAR x: INTEGER): INTEGER;
BEGIN RETURN Bump(x,2) END Forward;
PROCEDURE Work*(n: INTEGER): INTEGER;
VAR local: INTEGER;
BEGIN local:=n; g:=1;
    RETURN Forward(local)+local+Bump(g,3)+g
END Work;
PROCEDURE Change*(n:INTEGER):INTEGER;
BEGIN RETURN Bump(n,5)+n END Change;
PROCEDURE Same*(VAR a,b:INTEGER):INTEGER;
BEGIN a:=a+1; b:=b+2; RETURN a+b END Same;
PROCEDURE Alias*(n:INTEGER):INTEGER;
VAR x:INTEGER;
BEGIN x:=n; RETURN Same(x,x)+x END Alias;
END Ref.
"#;

#[test]
fn var_integer_formals_are_exclusively_addressable_scalar_arguments() {
    let module=lower_module(SOURCE).unwrap();
    let bump=&module.functions[0];
    assert_eq!(bump.parameters,["x","amount"]);
    assert_eq!(bump.parameter_by_ref,[true,false]);
    assert!(matches!(bump.body[0],Stmt::Assign {variable:0,..}));
    let forward=&module.functions[1];
    assert_eq!(forward.parameter_by_ref,[true]);
    assert!(matches!(&forward.body[0],Stmt::Return(Expr::Call {name,args})
        if name=="Bump" && matches!(args.as_slice(),[Expr::AddressOfScalar(0),Expr::Integer(2)])));
    let same=&module.functions[4];
    assert_eq!(same.parameter_by_ref,[true,true]);
    let alias=&module.functions[5];
    assert!(matches!(&alias.body[1],Stmt::Return(Expr::Binary(..))));
}

#[test]
fn var_integer_requires_checked_assignable_actual() {
    for source in [
        "MODULE Wrong;
         PROCEDURE Mut*(VAR x:INTEGER):INTEGER;
         BEGIN x:=1; RETURN x END Mut;
         PROCEDURE Caller*(n:INTEGER):INTEGER;
         BEGIN RETURN Mut(3) END Caller;
         END Wrong.",
        "MODULE Wrong;
         PROCEDURE Mut*(VAR x:INTEGER):INTEGER;
         BEGIN x:=1; RETURN x END Mut;
         PROCEDURE Caller*(n:INTEGER):INTEGER;
         BEGIN RETURN Mut(n+1) END Caller;
         END Wrong.",
        "MODULE Wrong;
         CONST C=1;
         PROCEDURE Mut*(VAR x:INTEGER):INTEGER;
         BEGIN x:=1; RETURN x END Mut;
         PROCEDURE Caller*(n:INTEGER):INTEGER;
         BEGIN RETURN Mut(C) END Caller;
         END Wrong.",
    ] {
        assert!(lower_module(source).is_err());
    }
}
