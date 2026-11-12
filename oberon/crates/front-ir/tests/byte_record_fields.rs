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
fn byte_only_objects_and_mixed_copy_project_exact_bytes() {
    let (module,_,layouts,objects)=lower_module_with_record_objects("MODULE Pure;
        TYPE R=RECORD b: BYTE END;
        VAR r: R;
        PROCEDURE F*(n: INTEGER): INTEGER;
        VAR l: R;
        BEGIN l.b:=n; r.b:=l.b; RETURN r.b END F;
        END Pure.").unwrap();
    assert_eq!((layouts[0].size_bytes,layouts[0].alignment_bytes),(1,1));
    assert_eq!(module.global_arrays[0].length,1);
    assert_eq!(module.functions[0].arrays[0].length,1);
    assert_eq!(objects.len(),2);
    assert!(matches!(module.functions[0].body[0],
        Stmt::AssignRecordField {array:0,byte_offset:0,bits:8,..}));

    let copy=lower_module("MODULE Copy;
        TYPE R=RECORD b: BYTE; n: INTEGER END;
        VAR x,y: R;
        PROCEDURE F*(n: INTEGER): INTEGER;
        BEGIN x.b:=n; x.n:=n+1; y:=x; RETURN y.b+y.n END F;
        END Copy.").unwrap();
    assert!(matches!(copy.functions[0].body[2],
        Stmt::CopyRecordBytes {
            dst_array:1,dst_byte_offset:0,src_array:0,
            src_byte_offset:0,byte_length:8,reverse:false,
        }));
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

#[test]
fn precise_byte_object_sizes_round_up_handles_without_forging_layout() {
    let src="MODULE Tiny;
        TYPE Three=RECORD a,b,c: BYTE END;
             Five=RECORD a,b,c,d,e: BYTE END;
        VAR g: Three;
        PROCEDURE Test*(n: INTEGER): INTEGER;
        VAR x: Five;
        BEGIN
            x.a:=n; x.e:=n+4; g.b:=x.e;
            RETURN g.a+g.b+x.a
        END Test;
        END Tiny.";
    let (module,_,layouts,objects)=lower_module_with_record_objects(src).unwrap();
    assert_eq!(layouts[0].size_bytes,3);
    assert_eq!(layouts[1].size_bytes,5);
    assert_eq!(layouts[0].alignment_bytes,1);
    assert_eq!(layouts[1].alignment_bytes,1);
    assert_eq!(module.global_arrays[0].length,1);
    assert_eq!(module.functions[0].arrays[0].length,2);
    assert_eq!(objects.len(),2);
    assert!(matches!(&module.functions[0].body[1],
        Stmt::AssignRecordField {array:0,byte_offset:4,bits:8,..}));
}

#[test]
fn byte_subrecord_copies_carry_complete_regions_including_padding() {
    let source="MODULE Mem;
        TYPE Byte3=RECORD a,b,c: BYTE END;
        Pair=RECORD left,right: Byte3; count: INTEGER END;
        VAR global: Pair;
        PROCEDURE Go*(n: INTEGER): INTEGER;
        VAR local: Pair; child: Byte3;
        BEGIN
            child.a:=n; child.b:=n+1; child.c:=n+2;
            local.left:=child;
            local.right:=local.left;
            local.left:=local.right;
            local.count:=n;
            global:=local;
            RETURN global.left.a+global.right.c+global.count
        END Go;
        END Mem.";
    let (mod_,_,layouts,_)=lower_module_with_record_objects(source).unwrap();
    let pair=layouts.iter().find(|r|r.declaration=="Mem.Pair").unwrap();
    assert_eq!((pair.size_bytes,pair.alignment_bytes),(12,4));
    assert_eq!(pair.fields.iter().map(|f|f.offset_bytes).collect::<Vec<_>>(),
        [0,3,8]);
    let instructions=&mod_.functions[0].body;
    assert!(matches!(instructions[3],Stmt::CopyRecordBytes {
        dst_array:0,dst_byte_offset:0,src_array:1,
        src_byte_offset:0,byte_length:3,reverse:false,
    }));
    assert!(matches!(instructions[4],Stmt::CopyRecordBytes {
        dst_array:0,dst_byte_offset:3,src_array:0,
        src_byte_offset:0,byte_length:3,reverse:false,
    }));
    assert!(matches!(instructions[5],Stmt::CopyRecordBytes {
        dst_array:0,dst_byte_offset:0,src_array:0,
        src_byte_offset:3,byte_length:3,reverse:false,
    }));
    assert!(matches!(instructions[7],Stmt::CopyRecordBytes {
        dst_array:2,dst_byte_offset:0,src_array:0,
        src_byte_offset:0,byte_length:12,reverse:false,
    }));
}

#[test]
fn derived_to_base_byte_prefix_copies_only_base_extent() {
    let source="MODULE Prefix;
        TYPE Base=RECORD tag: BYTE END;
             Derived=RECORD (Base) value: INTEGER END;
        VAR b: Base; d: Derived;
        PROCEDURE Go*(n: INTEGER): INTEGER;
        BEGIN
            d.tag:=n; d.value:=n+1; b:=d;
            RETURN b.tag+d.value
        END Go;
        END Prefix.";
    let (module,_,layouts,_)=lower_module_with_record_objects(source).unwrap();
    assert_eq!(layouts.iter().find(|r|r.declaration=="Prefix.Base").unwrap().size_bytes,1);
    assert_eq!(layouts.iter().find(|r|r.declaration=="Prefix.Derived").unwrap().size_bytes,8);
    assert!(matches!(module.functions[0].body[2],Stmt::CopyRecordBytes {
        dst_array:0,dst_byte_offset:0,src_array:1,
        src_byte_offset:0,byte_length:1,reverse:false,
    }));
}
