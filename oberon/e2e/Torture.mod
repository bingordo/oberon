MODULE Torture;
  IMPORT Out;

  CONST W = 8;

  TYPE
    Base = RECORD a: INTEGER END;
    Derived = RECORD (Base) b: INTEGER END;
    Ref = POINTER TO Base;
    RefD = POINTER TO Derived;
    Vec = ARRAY 4 OF INTEGER;
    Mat = ARRAY 2, 3 OF INTEGER;

  VAR
    n, k: INTEGER; r: REAL; s: SET; v: Vec; m: Mat;
    d: Derived; p: Ref; ch: CHAR; text: ARRAY 8 OF CHAR;
    fn: PROCEDURE (x: INTEGER): INTEGER;

  PROCEDURE twice(x: INTEGER): INTEGER;
  BEGIN RETURN x * 2 END twice;

  PROCEDURE bump(VAR x: INTEGER);
  BEGIN INC(x, 10) END bump;

  PROCEDURE sum(a: ARRAY OF INTEGER): INTEGER;
    VAR i, t: INTEGER;
  BEGIN
    t := 0;
    FOR i := 0 TO LEN(a) - 1 DO t := t + a[i] END;
    RETURN t
  END sum;

  PROCEDURE pick(VAR b: Base; which: INTEGER): INTEGER;
    VAR out: INTEGER;
  BEGIN
    IF which = 0 THEN
      out := b.a
    ELSE
      CASE b OF
        Base: out := 10
      | Derived: out := b.a + 100
      | : out := 0
      END
    END;
    RETURN out
  END pick;

  PROCEDURE outer();
    VAR acc: INTEGER;
    PROCEDURE inner();
    BEGIN acc := acc + 7 END inner;
    PROCEDURE deeper();
      PROCEDURE deepest();
      BEGIN acc := acc * 2 END deepest;
    BEGIN deepest() END deeper;
  BEGIN
    acc := 1;
    inner();
    deeper();
    Out.WriteInt(acc, W); Out.WriteLn
  END outer;

  PROCEDURE line1();
  BEGIN
    Out.WriteInt(17 DIV 5, W); Out.WriteInt(17 MOD 5, W);
    Out.WriteInt((0 - 7) DIV 3, W); Out.WriteInt((0 - 7) MOD 3, W);
    Out.WriteInt(7 DIV (0 - 3), W); Out.WriteInt(7 MOD (0 - 3), W);
    Out.WriteLn
  END line1;

  PROCEDURE line2();
  BEGIN
    r := 2.5 * 4.0;
    Out.WriteInt(FLOOR(r), W);
    Out.WriteInt(FLOOR(-1.5), W);
    Out.WriteInt(FLOOR(FLT(7) + 0.5), W);
    Out.WriteInt(ABS(-9), W);
    Out.WriteInt(ORD(ODD(-3)), W);
    Out.WriteLn
  END line2;

  PROCEDURE line3();
  BEGIN
    s := {1 .. 3} + {5};
    Out.WriteInt(ORD(2 IN s), W);
    Out.WriteInt(ORD(4 IN s), W);
    Out.WriteInt(ORD(0 IN s), W);
    s := s * {3 .. 6};
    Out.WriteInt(ORD(3 IN s), W);
    s := -{0};
    Out.WriteInt(ORD(31 IN s), W);
    Out.WriteLn
  END line3;

  PROCEDURE line4();
  BEGIN
    Out.WriteInt(ORD(LSL(1, 31) < 0), W);
    Out.WriteInt(ASR(-8, 1), W);
    Out.WriteInt(ORD(ROR(1, 1) < 0), W);
    Out.WriteInt(ORD(ch), W);
    ch := CHR(66);
    Out.WriteInt(ORD(ch), W);
    Out.WriteLn
  END line4;

  PROCEDURE line5();
  BEGIN
    text := "abc";
    Out.WriteInt(ORD(text < "abd"), W);
    Out.WriteInt(ORD(text = "abc"), W);
    Out.WriteInt(ORD(text < "ab"), W);
    Out.WriteInt(ORD("a" <= ch), W);
    Out.WriteLn
  END line5;

  PROCEDURE line6();
  BEGIN
    d.a := 3; d.b := 4;
    Out.WriteInt(pick(d, 0), W);
    Out.WriteInt(pick(d, 1), W);
    NEW(p);
    p.a := 21;
    Out.WriteInt(p.a, W);
    Out.WriteInt(ORD(p IS RefD), W);
    Out.WriteInt(ORD(p IS Derived), W);
    Out.WriteLn
  END line6;

  PROCEDURE line7();
    VAR i, j, t: INTEGER;
  BEGIN
    i := 0; t := 0;
    WHILE i < 6 DO t := t + i; INC(i) ELSIF i < 10 DO INC(i) END;
    FOR j := 0 TO 3 DO v[j] := j * j END;
    FOR j := 3 TO 0 BY -1 DO t := t + v[j] END;
    m[1, 2] := 42;
    REPEAT DEC(i) UNTIL i = 0;
    Out.WriteInt(t, W); Out.WriteInt(sum(v), W); Out.WriteInt(m[1, 2], W);
    Out.WriteInt(v[2], W); Out.WriteInt(i, W);
    Out.WriteLn
  END line7;

  PROCEDURE line8();
  BEGIN
    n := 5;
    bump(n);
    Out.WriteInt(n, W);
    fn := twice;
    Out.WriteInt(fn(21), W);
    Out.WriteLn
  END line8;


  PROCEDURE take(b: Base): INTEGER;
  BEGIN
    (* A value parameter of record type is read-only *and* a copy (report 10.1,
       9.1); it can be read here, which is what the caller's copy is for. *)
    RETURN b.a
  END take;

  PROCEDURE col(s: ARRAY OF CHAR): INTEGER;
    VAR i: INTEGER;
  BEGIN
    i := 0;
    WHILE (i < LEN(s)) & (s[i] # 0X) DO INC(i) END;
    RETURN i
  END col;

  PROCEDURE line9();
    VAR q: RefD;
  BEGIN
    NEW(q);
    q.a := 7;
    q.b := 8;
    p := q;
    (* A type guard reached through a pointer, in both spellings the report
       itself uses: a record name (report 8.1) and a pointer name (9.5). *)
    Out.WriteInt(p(Derived).b, W);
    Out.WriteInt(p(RefD).b, W);
    Out.WriteInt(p.a, W);
    Out.WriteLn
  END line9;

  PROCEDURE line10();
  BEGIN
    (* A type CASE: inside the arm the case variable has the label's type, so
       the derived field is reachable (report 9.5). *)
    CASE p OF
      RefD: Out.WriteInt(p.b, W)
    | Base: Out.WriteInt(0 - p.a, W)
    | : Out.WriteInt(0, W)
    END;
    Out.WriteLn
  END line10;

  PROCEDURE line11();
    VAR bits: SET; mant: REAL; e: INTEGER;
  BEGIN
    bits := {};
    INCL(bits, 3); INCL(bits, 5); EXCL(bits, 5);
    Out.WriteInt(ORD(3 IN bits), W); Out.WriteInt(ORD(5 IN bits), W);
    mant := 3.0; e := 0;
    PACK(mant, e);
    UNPK(mant, e);
    Out.WriteInt(e, W); Out.WriteInt(FLOOR(mant), W);
    Out.WriteLn
  END line11;

  PROCEDURE line12();
    VAR d2: Derived;
  BEGIN
    d2.a := 10; d2.b := 20;
    Out.WriteInt(take(d2), W);
    Out.WriteInt(d2.a + d2.b, W);
    Out.WriteLn
  END line12;

  PROCEDURE line13();
    VAR ch2: CHAR;
  BEGIN
    ch2 := "B";
    CASE ch2 OF
      "A": Out.WriteInt(1, W)
    | "B": Out.WriteInt(2, W)
    | : Out.WriteInt(3, W)
    END;
    Out.WriteInt(col("hello"), W);
    Out.WriteLn
  END line13;

  PROCEDURE Run*;
  BEGIN
    line1(); line2(); line3(); line4(); line5();
    line6(); line7(); line8(); outer();
    line9(); line10(); line11(); line12(); line13()
  END Run;

BEGIN
  n := 0; k := 1; ch := "A"
END Torture.
