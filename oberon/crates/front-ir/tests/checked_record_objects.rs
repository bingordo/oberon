use oberon_front_ir::{
    lower_module, lower_module_with_metadata, lower_module_with_record_objects,
};

#[test]
fn checked_record_object_bindings_preserve_global_local_scope_and_identity() {
    let source = "MODULE M;
        TYPE Pair = RECORD a,b: INTEGER END;
        VAR g,h: Pair;
        PROCEDURE F*(n: INTEGER): INTEGER;
        VAR x,y: Pair;
        BEGIN
            x.a := n; x.b := n+1; y := x; g := y;
            RETURN g.a + g.b
        END F;
        END M.";
    let (module, exports, layouts, objects) =
        lower_module_with_record_objects(source).unwrap();
    assert_eq!(exports, ["F"]);
    assert_eq!(module.global_arrays.len(), 2);
    assert_eq!(module.functions[0].arrays.len(), 2);
    assert_eq!(objects.len(), 4);
    assert_eq!(objects.iter().map(|r|r.name.as_str()).collect::<Vec<_>>(),
        ["g","h","x","y"]);
    assert_eq!(objects.iter().map(|r|r.function.as_deref()).collect::<Vec<_>>(),
        [None,None,Some("F"),Some("F")]);
    assert!(objects.iter().all(|r|r.record=="M.Pair"));
    assert_eq!(layouts.len(),1);
    assert_eq!(layouts[0].size_bytes,8);
    assert_eq!(lower_module(source).unwrap(), module);
    assert_eq!(lower_module_with_metadata(source).unwrap(),
        (module,exports,layouts));
}

#[test]
fn anonymous_record_globals_and_locals_get_distinct_checked_declarations() {
    let source = "MODULE Anon;
        VAR g: RECORD a,b: INTEGER END;
        PROCEDURE F*(n: INTEGER): INTEGER;
        VAR x: RECORD a,b: INTEGER END;
        BEGIN x.a:=n; x.b:=n+1; g.a:=x.a; RETURN g.a+x.b END F;
        END Anon.";
    let (_, _, layouts, objects) = lower_module_with_record_objects(source).unwrap();
    assert_eq!(objects.len(),2);
    assert_ne!(objects[0].record,objects[1].record);
    assert_eq!(objects[0].record,"Anon.global.g");
    assert_eq!(objects[1].record,"Anon.F.local.x");
    assert_eq!(layouts.len(),2);
    assert_eq!(layouts[0].size_bytes,8);
    assert_eq!(layouts[1].size_bytes,8);
}

#[test]
fn exported_record_objects_stay_rejected() {
    let source="MODULE Bad; TYPE Pair = RECORD a,b: INTEGER END;
        VAR g*: Pair; END Bad.";
    assert!(lower_module_with_record_objects(source).is_err());
}
