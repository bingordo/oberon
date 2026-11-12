use oberon_front_ir::{lower_module,Expr,Stmt};

#[test]
fn private_new_pointer_fields_alias_and_zeroed_memory_are_checked() {
    let source="MODULE HeapRecord;
        TYPE R=RECORD b: BYTE; value: INTEGER END;
             P=POINTER TO R;
        VAR g: P;
        PROCEDURE Work*(n: INTEGER): INTEGER;
        VAR p,q: P;
        BEGIN
            NEW(p);
            p^.b:=n; p^.value:=n+100;
            q:=p;
            NEW(g);
            g^.value:=q^.value;
            RETURN q^.b+g^.value
        END Work;
        END HeapRecord.";
    let module=lower_module(source).expect("checked private pointer module");
    assert_eq!(module.pointer_globals.len(),1);
    assert_eq!(module.pointer_globals[0].bytes,8);
    let f=&module.functions[0];
    assert_eq!(f.pointer_locals.len(),2);
    assert!(matches!(f.body[0],Stmt::NewPointer{pointer:0,bytes:8}));
    assert!(matches!(f.body[1],Stmt::AssignPointerField {
        pointer:0,byte_offset:0,bits:8,..
    }));
    assert!(matches!(f.body[2],Stmt::AssignPointerField {
        pointer:0,byte_offset:4,bits:32,..
    }));
    assert!(matches!(f.body[3],Stmt::AssignPointer{pointer:1,source:Some(0)}));
    assert!(matches!(f.body[4],Stmt::NewPointer{pointer:2,bytes:8}));
    assert!(matches!(f.body[5],Stmt::AssignPointerField {
        pointer:2,byte_offset:4,bits:32,
        value:Expr::PointerField {pointer:1,byte_offset:4,bits:32}
    }));
}

#[test]
fn named_private_pointer_can_be_cleared_and_reallocated() {
    let source="MODULE Clear;
        TYPE R=RECORD a:INTEGER END;
        VAR p: POINTER TO R;
        PROCEDURE Work*(n:INTEGER):INTEGER;
        BEGIN NEW(p); p^.a:=n; p:=NIL; NEW(p); RETURN p^.a END Work;
        END Clear.";
    let m=lower_module(source).unwrap();
    let f=&m.functions[0];
    assert!(matches!(f.body[2],Stmt::AssignPointer{pointer:0,source:None}));
}

#[test]
fn unsupported_pointer_surfaces_fail_closed() {
    assert!(lower_module("MODULE Bad;
        TYPE R=RECORD a:INTEGER END;
        VAR p*: POINTER TO R;
        END Bad.").is_err());
    assert!(lower_module("MODULE Bad;
        TYPE R=RECORD a:INTEGER END;
        VAR p: POINTER TO R;
        PROCEDURE Work*(n:INTEGER):INTEGER;
        BEGIN RETURN p^.missing END Work;
        END Bad.").is_err());
    assert!(lower_module("MODULE Bad;
        VAR p: POINTER TO INTEGER;
        END Bad.").is_err());
}
