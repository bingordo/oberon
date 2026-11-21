use oberon_front_ir::{lower_module, Expr, Stmt};

const SOURCE: &str = r#"
MODULE Ref;
VAR global: ARRAY 2, 3 OF INTEGER;
PROCEDURE Bump*(VAR x: INTEGER; delta: INTEGER): INTEGER;
BEGIN x := x + delta; RETURN x END Bump;
PROCEDURE Local*(n: INTEGER): INTEGER;
VAR cells: ARRAY 4 OF INTEGER; i: INTEGER;
BEGIN
    cells[0] := n; cells[1] := 2; i := 1;
    RETURN Bump(cells[i], cells[0])
END Local;
PROCEDURE Multi*(n: INTEGER): INTEGER;
BEGIN
    global[1,2] := n;
    RETURN Bump(global[1,2], 1)
END Multi;
PROCEDURE Nested*(n: INTEGER): INTEGER;
VAR cells: ARRAY 4 OF INTEGER;
BEGIN
    cells[0] := 1; cells[1] := n;
    RETURN Bump(cells[cells[0]], cells[1])
END Nested;
PROCEDURE Alias*(n: INTEGER): INTEGER;
VAR cells: ARRAY 4 OF INTEGER;
BEGIN
    cells[0] := n;
    RETURN Bump(cells[0], cells[0]) + Bump(cells[0], 1)
END Alias;
END Ref.
"#;

#[test]
fn checked_indexed_var_actuals_carry_storage_and_source_bounds() {
    let module=lower_module(SOURCE).unwrap();
    let local=&module.functions[1];
    assert_eq!(local.arrays[0].name,"cells");
    assert_eq!(local.arrays[0].length,4);
    assert!(matches!(&local.body[3], Stmt::Return(Expr::Call {name,args,..})
        if name=="Bump" && matches!(args.as_slice(),[
            Expr::AddressOfArrayElement {array:0,index},
            Expr::ArrayElement {array:0,..}
        ] if **index == Expr::Variable(1 + 1 + 0)))); // parameter n, local i

    let multi=&module.functions[2];
    assert!(multi.arrays.is_empty());
    assert!(matches!(&multi.body[1],Stmt::Return(Expr::Call {name,args,..})
        if name=="Bump" && matches!(args.as_slice(),[
            Expr::AddressOfMultiArrayElement {array:0,dims,indices},
            Expr::Integer(1),
        ] if dims == &[2,3] && indices == &[Expr::Integer(1),Expr::Integer(2)])));
}

#[test]
fn indexed_var_subscripts_are_lowered_once_as_expressions() {
    let module=lower_module(SOURCE).unwrap();
    let nested=&module.functions[3];
    assert!(matches!(&nested.body[2],Stmt::Return(Expr::Call {args,..})
        if matches!(args.first(),
            Some(Expr::AddressOfArrayElement {array:0,index})
                if matches!(index.as_ref(),
                    Expr::ArrayElement {array:0,index}
                        if index.as_ref()==&Expr::Integer(0)))));
    let alias=&module.functions[4];
    assert!(matches!(&alias.body[1],Stmt::Return(Expr::Binary(_,left,right))
        if matches!(left.as_ref(),Expr::Call {args,..}
            if matches!(args.as_slice(),[
                Expr::AddressOfArrayElement {array:0,..},Expr::ArrayElement {array:0,..}
            ])) &&
        matches!(right.as_ref(),Expr::Call {args,..}
            if matches!(args.first(),Some(Expr::AddressOfArrayElement {array:0,..})))));
}

#[test]
fn indexed_var_actuals_fail_closed_for_rvalues_and_non_integer_objects() {
    let sources=[
        "MODULE Wrong;
         PROCEDURE Bump(VAR x:INTEGER):INTEGER;
         BEGIN INC(x); RETURN x END Bump;
         PROCEDURE Test*(n:INTEGER):INTEGER;
         VAR cells:ARRAY 4 OF INTEGER;
         BEGIN RETURN Bump(cells[n]+1) END Test;
         END Wrong.",
        "MODULE Wrong;
         PROCEDURE Bump(VAR x:INTEGER):INTEGER;
         BEGIN INC(x); RETURN x END Bump;
         PROCEDURE Test*(n:INTEGER):INTEGER;
         VAR flags:ARRAY 4 OF BOOLEAN;
         BEGIN RETURN Bump(flags[n]) END Test;
         END Wrong.",
        "MODULE Wrong;
         PROCEDURE Bump(VAR x:INTEGER):INTEGER;
         BEGIN INC(x); RETURN x END Bump;
         PROCEDURE Test*(n:INTEGER):INTEGER;
         VAR cells:ARRAY 2,3 OF INTEGER;
         BEGIN RETURN Bump(cells[n]) END Test;
         END Wrong.",
    ];
    for source in sources {
        assert!(lower_module(source).is_err(),"unexpectedly accepted {source}");
    }
}
