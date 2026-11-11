use oberon_front_ir::{
    lower_module, lower_module_with_record_objects, CheckedRecordFieldType,
    Expr, Stmt,
};

const SOURCE:&str="MODULE ByteRec;
    TYPE Mixed = RECORD tag: BYTE; count: INTEGER; tail: BYTE END;
    VAR g: Mixed;
    PROCEDURE Sum*(n: INTEGER): INTEGER;
    VAR l: Mixed;
    BEGIN
        l.tag := n; l.count := n+100; l.tail := n+1;
        g.tag := l.tag; g.count := l.count; g.tail := l.tail;
        RETURN g.tag + g.count + g.tail
    END Sum;
    END ByteRec.";

#[test]
fn source_checked_mixed_byte_and_integer_has_padded_offsets_and_word_storage() {
    let (module,exports,layouts,objects)=lower_module_with_record_objects(SOURCE).unwrap();
    assert_eq!(exports,["Sum"]);
    let mixed=layouts.iter().find(|x|x.declaration=="ByteRec.Mixed").unwrap();
    assert_eq!((mixed.size_bytes,mixed.alignment_bytes),(12,4));
    assert_eq!(mixed.fields.iter().map(|f|f.offset_bytes).collect::<Vec<_>>(),
        [0,4,8]);
    assert!(matches!(mixed.fields[0].ty,CheckedRecordFieldType::Byte));
    assert!(matches!(mixed.fields[1].ty,CheckedRecordFieldType::Integer));
    assert!(matches!(mixed.fields[2].ty,CheckedRecordFieldType::Byte));
    assert_eq!(module.global_arrays[0].length,3);
    let f=&module.functions[0];
    assert_eq!(f.arrays[0].length,3);
    assert_eq!(objects.len(),2);
    assert!(matches!(&f.body[0],
        Stmt::AssignRecordField {array:0,byte_offset:0,bits:8,..}));
    assert!(matches!(&f.body[1],
        Stmt::AssignRecordField {array:0,byte_offset:4,bits:32,..}));
    assert!(matches!(&f.body[2],
        Stmt::AssignRecordField {array:0,byte_offset:8,bits:8,..}));
    assert!(matches!(&f.body[3],
        Stmt::AssignRecordField {array:1,byte_offset:0,bits:8,
            value:Expr::RecordField {array:0,byte_offset:0,bits:8}}));
    assert!(matches!(&f.body[4],
        Stmt::AssignRecordField {array:1,byte_offset:4,bits:32,
            value:Expr::RecordField {array:0,byte_offset:4,bits:32}}));
    assert_eq!(lower_module(SOURCE).unwrap(),module);
}

#[test]
fn byte_only_objects_and_unverified_byte_range_copy_fail_closed() {
    assert!(lower_module("MODULE Pure;
        TYPE R=RECORD b: BYTE END;
        VAR r: R;
        END Pure.").is_err());

    assert!(lower_module("MODULE Copy;
        TYPE R=RECORD b: BYTE; n: INTEGER END;
        VAR x,y: R;
        PROCEDURE F*(n: INTEGER): INTEGER;
        BEGIN x.b:=n; y:=x; RETURN y.b END F;
        END Copy.").is_err());
}

#[test]
fn nested_byte_fields_preserve_padding_and_absolute_offsets() {
    let src="MODULE NestedByte;
        TYPE Inner=RECORD b: BYTE; n: INTEGER END;
        Outer=RECORD front: BYTE; child: Inner; tail: INTEGER END;
        VAR g: Outer;
        PROCEDURE F*(n: INTEGER): INTEGER;
        BEGIN g.child.b:=n; g.child.n:=n+1; g.tail:=g.child.n;
            RETURN g.child.b + g.tail
        END F;
        END NestedByte.";
    let (module,_,layouts,_)=lower_module_with_record_objects(src).unwrap();
    let inner=layouts.iter().find(|x|x.declaration=="NestedByte.Inner").unwrap();
    let outer=layouts.iter().find(|x|x.declaration=="NestedByte.Outer").unwrap();
    assert_eq!(inner.size_bytes,8);
    assert_eq!(outer.size_bytes,16);
    assert_eq!(outer.fields.iter().map(|f|f.offset_bytes).collect::<Vec<_>>(),
        [0,4,12]);
    assert_eq!(module.global_arrays[0].length,4);
    let f=&module.functions[0];
    assert!(matches!(&f.body[0],
        Stmt::AssignRecordField {array:0,byte_offset:4,bits:8,..}));
    assert!(matches!(&f.body[1],
        Stmt::AssignRecordField {array:0,byte_offset:8,bits:32,..}));
}
