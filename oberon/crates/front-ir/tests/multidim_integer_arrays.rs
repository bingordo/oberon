use oberon_front_ir::{lower_module,Expr,Stmt,ArrayLocal,ArrayGlobal};

#[test]
fn checked_two_dimensional_integer_arrays_use_contiguous_storage() {
    let source="MODULE Matrix;
        CONST Rows=2; Cols=3;
        VAR g: ARRAY Rows, Cols OF INTEGER;
        PROCEDURE Work*(n: INTEGER): INTEGER;
        VAR local: ARRAY Rows, Cols OF INTEGER;
        BEGIN
            local[1,2]:=n;
            g[0,2]:=local[1,2]+1;
            RETURN g[0,2]
        END Work;
        END Matrix.";
    let m=lower_module(source).unwrap();
    assert_eq!(m.global_arrays,[
        ArrayGlobal{name:"g".into(),length:6,exported:false},
    ]);
    let f=&m.functions[0];
    assert_eq!(f.arrays,[
        ArrayLocal{name:"local".into(),length:6},
    ]);
    assert!(matches!(&f.body[0],Stmt::AssignMultiArray {
        array:0,dims,indices,..
    } if dims==&[2,3] && indices==&[Expr::Integer(1),Expr::Integer(2)]));
    assert!(matches!(&f.body[1],Stmt::AssignMultiArray {
        array:1,dims,indices,..
    } if dims==&[2,3] && indices==&[Expr::Integer(0),Expr::Integer(2)]));
}

#[test]
fn checked_three_dimensional_indexing_carries_all_source_bounds() {
    let source="MODULE Cube;
        VAR cube: ARRAY 2,3,4 OF INTEGER;
        PROCEDURE Work*(n: INTEGER): INTEGER;
        BEGIN cube[1,2,3]:=n; RETURN cube[1,2,3] END Work;
        END Cube.";
    let m=lower_module(source).unwrap();
    assert_eq!(m.global_arrays[0].length,24);
    assert!(matches!(&m.functions[0].body[0],
        Stmt::AssignMultiArray {dims,indices,..}
        if dims==&[2,3,4] && indices.len()==3));
}

#[test]
fn unsupported_array_dimensions_and_types_fail_closed() {
    assert!(lower_module("MODULE TooMany;
        VAR a: ARRAY 1024,2 OF INTEGER;
        END TooMany.").is_err());
    assert!(lower_module("MODULE WrongRank;
        VAR a: ARRAY 2,3 OF INTEGER;
        PROCEDURE Work*(n: INTEGER): INTEGER;
        BEGIN RETURN a[n] END Work;
        END WrongRank.").is_err());
    assert!(lower_module("MODULE FloatArray;
        VAR a: ARRAY 2,3 OF REAL;
        END FloatArray.").is_err());
}
