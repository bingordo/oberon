use oberon_front_ir::{
    checked_scalar_interface,lower_module_with_record_objects_and_interfaces,
    parse_interface,render_interface,Expr,Stmt,
};

const PROVIDER:&str=r#"
MODULE Arithmetic;
PROCEDURE Add*(a,b:INTEGER):INTEGER;
BEGIN RETURN a+b END Add;
PROCEDURE Bump*(VAR x:INTEGER; n:INTEGER):INTEGER;
BEGIN x:=x+n; RETURN x END Bump;
PROCEDURE Hidden(x:INTEGER):INTEGER;
BEGIN RETURN x+100 END Hidden;
END Arithmetic.
"#;

const CONSUMER:&str=r#"
MODULE Client;
IMPORT A:=Arithmetic;
VAR count:INTEGER;
PROCEDURE Compute*(n:INTEGER):INTEGER;
VAR x:INTEGER;
BEGIN
    x:=A.Add(n,2);
    count:=A.Bump(x,3);
    RETURN x+count
END Compute;
END Client.
"#;

#[test]
fn scalar_sym_roundtrip_contains_only_exported_checked_signatures() {
    let interface=checked_scalar_interface(PROVIDER).unwrap();
    assert_eq!(interface.module,"Arithmetic");
    assert_eq!(interface.items.len(),2);
    let encoded=render_interface(&interface);
    assert!(encoded.contains("PROC Add(INTEGER; INTEGER): INTEGER"));
    assert!(encoded.contains("PROC Bump(VAR INTEGER; INTEGER): INTEGER"));
    assert!(!encoded.contains("Hidden"));
    assert_eq!(parse_interface(&encoded).unwrap(),interface);
}

#[test]
fn import_alias_resolves_to_defining_module_symbol_and_checked_var_modes() {
    let interface=checked_scalar_interface(PROVIDER).unwrap();
    let (client,_,_,_)=lower_module_with_record_objects_and_interfaces(
        CONSUMER,&[interface]).unwrap();
    assert_eq!(client.imported_procedures.len(),2);
    assert_eq!(client.imported_procedures[1].parameter_by_ref,[true,false]);
    let body=&client.functions[0].body;
    assert!(matches!(&body[0],Stmt::Assign {
        value:Expr::ImportedCall{module,name,args},..
    } if module=="Arithmetic" && name=="Add" && args.len()==2));
    assert!(matches!(&body[1],Stmt::Assign {
        value:Expr::ImportedCall{module,name,args},..
    } if module=="Arithmetic" && name=="Bump"
        && matches!(args.as_slice(),[Expr::AddressOfScalar(_),Expr::Integer(3)])));
}

#[test]
fn import_fails_closed_for_missing_interface_private_proc_and_invalid_var_actual() {
    let interface=checked_scalar_interface(PROVIDER).unwrap();
    assert!(lower_module_with_record_objects_and_interfaces(CONSUMER,&[]).is_err());
    let private=CONSUMER.replace("A.Add(n,2)","A.Hidden(n)");
    assert!(lower_module_with_record_objects_and_interfaces(
        &private,&[interface.clone()]).is_err());
    let wrong=CONSUMER.replace("A.Bump(x,3)","A.Bump(n+1,3)");
    assert!(lower_module_with_record_objects_and_interfaces(&wrong,&[interface]).is_err());
}

#[test]
fn interface_writer_rejects_incomplete_non_procedure_exports() {
    assert!(checked_scalar_interface(
        "MODULE Bad; VAR x*:INTEGER; END Bad.").is_err());
}
