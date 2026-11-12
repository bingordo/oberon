use oberon_front_ir::{lower_module,lower_module_with_metadata,Expr,Stmt,CheckedRecordFieldType};

const SOURCE:&str=r#"
MODULE Linked;
TYPE Link=POINTER TO Cell;
     Cell=RECORD value:INTEGER; next:Link END;
VAR head:Link;
PROCEDURE Run*(n:INTEGER):INTEGER;
VAR p,q:Link; total:INTEGER;
BEGIN
    NEW(p);
    p^.value:=n;
    NEW(p^.next);
    p^.next^.value:=n+1;
    q:=p^.next;
    head:=p;
    total:=0;
    WHILE q#NIL DO
        total:=total+q^.value;
        q:=q^.next
    END;
    RETURN total+head^.value
END Run;
END Linked.
"#;

#[test]
fn recursive_pointer_record_is_finite_and_has_natural_offsets() {
    let (module,_,layouts)=lower_module_with_metadata(SOURCE).unwrap();
    let cell=layouts.iter().find(|layout|layout.declaration=="Linked.Cell").unwrap();
    assert_eq!((cell.size_bytes,cell.alignment_bytes),(16,8));
    assert_eq!(cell.fields[0].offset_bytes,0);
    assert_eq!(cell.fields[1].offset_bytes,8);
    assert!(matches!(&cell.fields[1].ty,CheckedRecordFieldType::Pointer(id)
        if id=="Linked.Cell"));
    assert_eq!(module.pointer_globals.len(),1);
    assert_eq!(module.pointer_globals[0].bytes,16);
    assert_eq!(module.pointer_globals[0].fields[1].bits,64);
    assert_eq!(module.pointer_globals[0].fields[1].target_record.as_deref(),Some("Linked.Cell"));
    let run=&module.functions[0];
    assert_eq!(run.pointer_locals.len(),2);
    assert!(matches!(run.body[2],Stmt::NewPointerLink {
        pointer:0,ref hops,byte_offset:8,bytes:16
    } if hops.is_empty()));
    assert!(matches!(run.body[3],Stmt::AssignPointerChainField {
        pointer:0,ref hops,byte_offset:0,bits:32,..
    } if hops==&[8]));
    assert!(matches!(&run.body[4],Stmt::AssignPointerValue {
        pointer:1,value:Expr::PointerValue {
            pointer:0,hops,byte_offset:Some(8),
        }
    } if hops.is_empty()));
}

#[test]
fn recursive_link_reassignment_and_nil_chain_are_accepted() {
    let source=r#"
MODULE List;
TYPE Link=POINTER TO Node; Node=RECORD data:INTEGER; next:Link END;
VAR first:Link;
PROCEDURE Run*(n:INTEGER):INTEGER;
VAR last:Link;
BEGIN
    NEW(first); first^.data:=n;
    NEW(last); last^.data:=n+2;
    first^.next:=last;
    last^.next:=NIL;
    IF first^.next#NIL THEN RETURN first^.next^.data
    ELSE RETURN 0 END
END Run;
END List.
"#;
    let m=lower_module(source).unwrap();
    let b=&m.functions[0].body;
    assert!(b.iter().any(|stmt|matches!(stmt,Stmt::AssignPointerChainField {
        bits:64,..
    })));
    assert!(b.iter().any(|stmt|matches!(stmt,Stmt::If {
        condition:Expr::PointerNullTest {equal:false,..},..
    })));
}

#[test]
fn recursive_pointer_chain_rejects_invalid_field_and_unverified_target() {
    let invalid=r#"
MODULE Wrong;
TYPE Link=POINTER TO Node; Node=RECORD data:INTEGER; next:Link END;
VAR p:Link;
PROCEDURE F*(n:INTEGER):INTEGER;
BEGIN NEW(p); RETURN p^.next^.missing END F;
END Wrong.
"#;
    assert!(lower_module(invalid).is_err());
    let invalid=r#"
MODULE Wrong;
TYPE Link=POINTER TO Node; Node=RECORD data:INTEGER; next:Link END;
VAR p:Link;
PROCEDURE F*(n:INTEGER):INTEGER;
BEGIN NEW(p); RETURN p^.data^.next END F;
END Wrong.
"#;
    assert!(lower_module(invalid).is_err());
}
