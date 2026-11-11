use oberon_front_ir::{lower_module, lower_module_with_exports,
    lower_module_with_metadata, CheckedRecordFieldType};

#[test]
fn source_checked_record_layouts_preserve_nested_identity_and_offsets() {
    let source = "MODULE M;
        TYPE Pair = RECORD a,b: INTEGER END;
        Tree = RECORD (Pair) inner: Pair; leaf: RECORD z: INTEGER END END;
        VAR tree: Tree;
        PROCEDURE Read*(n: INTEGER): INTEGER;
        BEGIN
            tree.inner.a := n;
            tree.leaf.z := tree.inner.a + tree.b;
            RETURN tree.leaf.z
        END Read;
        END M.";
    let (module, exports, layouts) = lower_module_with_metadata(source).unwrap();
    assert_eq!(exports, ["Read"]);
    assert_eq!(lower_module(source).unwrap(), module);
    assert_eq!(lower_module_with_exports(source).unwrap(), (module, exports));
    let pair = layouts.iter().find(|r| r.declaration == "M.Pair").unwrap();
    assert_eq!((pair.size_bytes, pair.alignment_bytes), (8, 4));
    assert_eq!(pair.fields.iter().map(|f| f.offset_bytes).collect::<Vec<_>>(), [0, 4]);
    let tree = layouts.iter().find(|r| r.declaration == "M.Tree").unwrap();
    assert_eq!(tree.base.as_deref(), Some("M.Pair"));
    assert_eq!(tree.fields.iter().map(|f| f.offset_bytes).collect::<Vec<_>>(), [8, 16]);
    assert_eq!((tree.size_bytes, tree.alignment_bytes), (20, 4));
    assert!(matches!(&tree.fields[0].ty,
        CheckedRecordFieldType::Record(id) if id == "M.Pair"));
    let anonymous = layouts.iter().find(|r| r.declaration == "M.Tree.leaf").unwrap();
    assert_eq!(anonymous.fields[0].offset_bytes, 0);
    assert_eq!(anonymous.size_bytes, 4);
    assert_ne!(tree.fields[0].declaration, tree.fields[1].declaration);
}

#[test]
fn aliases_preserve_nominal_identity_without_duplicate_layouts() {
    let source = "MODULE Alias;
        TYPE Pair = RECORD a,b: INTEGER END;
        Same = Pair;
        VAR p: Same;
        PROCEDURE F*(n: INTEGER): INTEGER;
        BEGIN p.a:=n; RETURN p.a END F;
        END Alias.";
    let (_, _, layouts) = lower_module_with_metadata(source).unwrap();
    assert_eq!(layouts.len(), 1);
    assert_eq!(layouts[0].declaration, "Alias.Pair");
}

#[test]
fn unsupported_source_types_do_not_leak_partial_layout() {
    let source = "MODULE Bad;
        TYPE R = RECORD a: INTEGER; b: REAL END;
        VAR r: R;
        END Bad.";
    assert!(lower_module_with_metadata(source).is_err());
}
