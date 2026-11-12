use oberon_front_ir::{lower_module,Expr,Stmt};

#[test]
fn module_and_procedure_constants_are_evaluated_by_checked_oberon_semantics() {
    let source="MODULE NativeConstants;
        CONST Size = 2 + 3; Double = 2 * Size; Enabled = TRUE;
        VAR data: ARRAY Double OF INTEGER;
        PROCEDURE Work*(n: INTEGER): INTEGER;
        CONST Size = 2; LocalSize = Size + 2;
        VAR local: ARRAY LocalSize OF INTEGER;
        BEGIN
            local[Size] := n + Double;
            IF Enabled THEN data[Size] := local[Size] END;
            RETURN data[Size] + LocalSize
        END Work;
        BEGIN data[0] := Double
        END NativeConstants.";
    let module=lower_module(source).expect("semantically checked constant source");
    assert_eq!(module.global_arrays[0].length,10);
    let f=&module.functions[0];
    assert_eq!(f.arrays[0].length,4);
    assert!(matches!(&f.body[0],Stmt::AssignArray {
        array:0,index:Expr::Integer(2),value:Expr::Binary(..)
    }));
    assert!(matches!(&f.body[1],Stmt::If {
        condition:Expr::Boolean(true),..
    }));
    assert!(matches!(&f.body[2],Stmt::Return(Expr::Binary(..))));
    let init=module.initializer.as_ref().unwrap();
    assert!(matches!(&init.body[0],Stmt::AssignArray {
        array:0,index:Expr::Integer(0),value:Expr::Integer(10)
    }));
}

#[test]
fn checker_constant_folding_accepts_wrapping_integer_expressions_and_named_bounds() {
    let source="MODULE ConstOps;
        CONST Wrap = 2147483647 + 1; N = 2 * 3;
        VAR data: ARRAY N OF INTEGER;
        PROCEDURE Work*(n: INTEGER): INTEGER;
        BEGIN data[5] := n; RETURN Wrap + data[5] END Work;
        END ConstOps.";
    let module=lower_module(source).unwrap();
    assert_eq!(module.global_arrays[0].length,6);
    assert!(matches!(&module.functions[0].body[1],
        Stmt::Return(Expr::Binary(..))));
}

#[test]
fn non_native_constants_and_invalid_lengths_fail_closed() {
    assert!(lower_module("MODULE Unsupported;
        CONST Pi = 3.5;
        PROCEDURE Work*(): INTEGER;
        BEGIN RETURN 1 END Work;
        END Unsupported.").is_err());
    assert!(lower_module("MODULE BadExtent;
        CONST N = 0;
        VAR x: ARRAY N OF INTEGER;
        END BadExtent.").is_err());
    assert!(lower_module("MODULE BadExtent;
        CONST N = 2048;
        VAR x: ARRAY N OF INTEGER;
        END BadExtent.").is_err());
}
