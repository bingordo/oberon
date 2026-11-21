use oberon_front_ir::{
    checked_scalar_interface,lower_module,lower_module_with_record_objects_and_interfaces,
    render_interface,Expr,Stmt,
};

const SOURCE:&str=r#"
MODULE Truth;
VAR global: BOOLEAN;
PROCEDURE Flip*(VAR b: BOOLEAN): BOOLEAN;
BEGIN b := ~b; RETURN b END Flip;
PROCEDURE Forward*(VAR flag: BOOLEAN): BOOLEAN;
BEGIN RETURN Flip(flag) END Forward;
PROCEDURE Same*(VAR a,b: BOOLEAN): BOOLEAN;
BEGIN a := ~a; b := ~b; RETURN a END Same;
PROCEDURE Probe*(n: INTEGER): INTEGER;
VAR local: BOOLEAN;
BEGIN
  local := n > 0;
  global := FALSE;
  IF Flip(local) THEN
    IF Forward(global) THEN RETURN 11 ELSE RETURN 12 END
  ELSE
    IF Same(global,global) THEN RETURN 13 ELSE RETURN 14 END
  END
END Probe;
END Truth.
"#;

#[test]
fn checked_var_boolean_formals_are_addressed_without_losing_boolean_types() {
    let module=lower_module(SOURCE).unwrap();
    assert_eq!(module.functions.len(),4);
    let flip=&module.functions[0];
    assert_eq!(flip.parameter_by_ref,[true]);
    assert_eq!(flip.boolean_variables,[0,1]); // formal and private global
    assert!(flip.returns_boolean);
    assert!(matches!(&flip.body[0],Stmt::Assign {variable:0,value:Expr::Not(_)}));

    let forward=&module.functions[1];
    assert_eq!(forward.parameter_by_ref,[true]);
    assert!(matches!(&forward.body[0],Stmt::Return(Expr::Call{
        name,args,boolean_arguments,returns_boolean:true,
    }) if name=="Flip" && boolean_arguments==&[true]
        && args==&[Expr::AddressOfScalar(0)]));

    let same=&module.functions[2];
    assert_eq!(same.parameter_by_ref,[true,true]);
    assert_eq!(same.boolean_variables,[0,1,2]);

    let probe=&module.functions[3];
    assert_eq!(probe.boolean_variables,[1,2]); // local and global
    assert!(matches!(&probe.body[2],Stmt::If {condition:Expr::Call {
        args,boolean_arguments,..
    },then_body,else_body}
        if args==&[Expr::AddressOfScalar(1)] &&
           boolean_arguments==&[true] &&
           matches!(then_body.first(),Some(Stmt::If{
               condition:Expr::Call{args,..},..
           }) if args==&[Expr::AddressOfScalar(2)]) &&
           matches!(else_body.first(),Some(Stmt::If{
               condition:Expr::Call{args,..},..
           }) if args==&[Expr::AddressOfScalar(2),Expr::AddressOfScalar(2)])));
}

#[test]
fn var_boolean_is_preserved_in_checked_imported_interfaces() {
    let sym=checked_scalar_interface(SOURCE).unwrap();
    let text=render_interface(&sym);
    assert!(text.contains("PROC Flip(VAR BOOLEAN): BOOLEAN"));
    assert!(text.contains("PROC Same(VAR BOOLEAN; VAR BOOLEAN): BOOLEAN"));
    let roundtrip=oberon_front_ir::parse_interface(&text).unwrap();
    assert_eq!(roundtrip,sym);

    let consumer=r#"
MODULE Consumer;
IMPORT Truth;
PROCEDURE Use*(n:INTEGER):INTEGER;
VAR flag:BOOLEAN;
BEGIN
  flag:=n>0;
  IF Truth.Flip(flag) THEN RETURN 1 ELSE RETURN 0 END
END Use;
END Consumer.
"#;
    let (module,_,_,_)=lower_module_with_record_objects_and_interfaces(
        consumer,&[sym]).unwrap();
    let imported=module.imported_procedures.iter()
        .find(|p|p.name=="Flip").unwrap();
    assert_eq!(imported.parameter_by_ref,[true]);
    assert_eq!(imported.boolean_parameters,[true]);
    assert!(imported.returns_boolean);
    assert!(matches!(&module.functions[0].body[1],Stmt::If{
        condition:Expr::ImportedCall{args,boolean_arguments,..},..
    } if args==&[Expr::AddressOfScalar(1)] && boolean_arguments==&[true]));
}

#[test]
fn var_boolean_actuals_reject_rvalues_and_wrong_types() {
    for caller in [
        "RETURN Flip(TRUE)",
        "RETURN Flip(~b)",
        "RETURN Flip(n)",
        "RETURN Flip(b & TRUE)",
    ] {
        let source=format!(
            "MODULE Wrong;
             PROCEDURE Flip(VAR flag:BOOLEAN):BOOLEAN;
             BEGIN flag:=~flag; RETURN flag END Flip;
             PROCEDURE Caller*(n:INTEGER):BOOLEAN;
             VAR b:BOOLEAN;
             BEGIN b:=TRUE; {caller} END Caller;
             END Wrong.");
        assert!(lower_module(&source).is_err(),"accepted invalid VAR actual: {caller}");
    }
    assert!(lower_module(
        "MODULE Wrong;
         PROCEDURE Flip(VAR flag:BOOLEAN):BOOLEAN;
         BEGIN flag:=~flag; RETURN flag END Flip;
         PROCEDURE Caller*(n:INTEGER):BOOLEAN;
         VAR a:ARRAY 4 OF BOOLEAN;
         BEGIN RETURN Flip(a[n]) END Caller;
         END Wrong."
    ).is_err());
}
