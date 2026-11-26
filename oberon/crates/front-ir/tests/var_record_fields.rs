//! Checked INTEGER record leaves passed by VAR across local and imported calls.
//! Storage identities and authoritative field offsets come from source sema.
use oberon_front_ir::{
    checked_scalar_interface,lower_module,lower_module_with_record_objects_and_interfaces,
    Expr,Stmt,
};

const SOURCE:&str=r#"
MODULE RecordVar;
TYPE Pair = RECORD a,b: INTEGER END;
     Nested = RECORD inner: Pair; tail: INTEGER END;
     Mixed = RECORD prefix: BYTE; count: INTEGER; suffix: BYTE END;
VAR g: Pair;
PROCEDURE Bump*(VAR x: INTEGER; d: INTEGER): INTEGER;
BEGIN INC(x,d); RETURN x END Bump;
PROCEDURE Forward*(VAR x: INTEGER; d: INTEGER): INTEGER;
BEGIN RETURN Bump(x,d) END Forward;
PROCEDURE Run*(n: INTEGER): INTEGER;
VAR p: Pair; q: Nested; mixed: Mixed;
BEGIN
  p.a:=n; p.b:=1; q.inner.a:=n;
  mixed.prefix:=2; mixed.count:=n; mixed.suffix:=3;
  RETURN Forward(p.a,3) + Bump(q.inner.a,4) +
         Forward(mixed.count,5) + Bump(g.a,6)
END Run;
END RecordVar.
"#;

/// Visit only call operands; arithmetic nesting must not change ABI identity.
fn call_actuals(expr:&Expr,out:&mut Vec<Expr>) {
    match expr {
        Expr::Call{args,..}|Expr::ImportedCall{args,..}=>{
            out.push(args[0].clone());
        }
        Expr::Binary(_,left,right)=>{
            call_actuals(left,out);
            call_actuals(right,out);
        }
        _=>{}
    }
}

#[test]
fn record_field_var_lowers_to_checked_existing_native_storage_addresses() {
    let module=lower_module(SOURCE).unwrap();
    let run=&module.functions[2];
    assert_eq!(run.arrays.len(),3);
    assert_eq!(run.global_arrays.len(),1);
    let Stmt::Return(result)=run.body.last().unwrap() else {
        panic!("missing return");
    };
    let mut actuals=Vec::new();
    call_actuals(result,&mut actuals);
    assert_eq!(actuals,[
        Expr::AddressOfArrayElement {
            array:0,index:Box::new(Expr::Integer(0)),
        },
        Expr::AddressOfArrayElement {
            array:1,index:Box::new(Expr::Integer(0)),
        },
        Expr::AddressOfRecordField {
            array:2,byte_offset:4,bits:32,
        },
        Expr::AddressOfArrayElement {
            array:3,index:Box::new(Expr::Integer(0)),
        },
    ]);
    assert!(matches!(&module.functions[1].body[0],
        Stmt::Return(Expr::Call{name,args,..}) if name=="Bump"
            && matches!(args[0],Expr::AddressOfScalar(0))));
}

#[test]
fn imported_var_integer_accepts_checked_record_field_and_byte_layout() {
    let provider="MODULE Ops;
        PROCEDURE Bump*(VAR n:INTEGER; delta:INTEGER):INTEGER;
        BEGIN INC(n,delta); RETURN n END Bump;
        END Ops.";
    let source="MODULE UseRecord;
        IMPORT O:=Ops;
        TYPE Mixed=RECORD first:BYTE; value:INTEGER END;
        VAR shared: Mixed;
        PROCEDURE Run*(n:INTEGER):INTEGER;
        BEGIN shared.first:=3; shared.value:=n;
            RETURN O.Bump(shared.value,7)+shared.value
        END Run;
        END UseRecord.";
    let interface=checked_scalar_interface(provider).unwrap();
    let (module,_,_,_)=lower_module_with_record_objects_and_interfaces(
        source,&[interface]).unwrap();
    let Stmt::Return(Expr::Binary(_,left,_))=
        module.functions[0].body.last().unwrap() else {
        panic!("missing expression");
    };
    assert!(matches!(left.as_ref(),
        Expr::ImportedCall{module,name,args,..}
        if module=="Ops" && name=="Bump" &&
            matches!(args.as_slice(),[
                Expr::AddressOfRecordField{array:0,byte_offset:4,bits:32},
                Expr::Integer(7)
            ])));
}

#[test]
fn record_var_rejects_aggregate_byte_and_nonlvalue_actuals() {
    let source="MODULE Wrong;
        TYPE R=RECORD small:BYTE; value:INTEGER END;
        VAR r:R;
        PROCEDURE Bump(VAR n:INTEGER):INTEGER;
        BEGIN INC(n); RETURN n END Bump;
        PROCEDURE Run*(n:INTEGER):INTEGER;
        BEGIN RETURN Bump(r.value) END Run;
        END Wrong.";
    for invalid in [
        source.replace("Bump(r.value)","Bump(r)"),
        source.replace("Bump(r.value)","Bump(r.small)"),
        source.replace("Bump(r.value)","Bump(r.value+1)"),
        source.replace("Bump(r.value)","Bump(r.unknown)"),
    ] {
        assert!(lower_module(&invalid).is_err(),"accepted invalid VAR actual: {invalid}");
    }
}
