MODULE Regress;
  IMPORT Out;

  CONST W = 8; A = 2; Bc = A + 1; C1 = 7 DIV 3;

  TYPE
    NodePtr = POINTER TO Node;
    Node = RECORD next: NodePtr; key: INTEGER END;
    Base = RECORD a: INTEGER END;
    Derived = RECORD (Base) b: INTEGER END;
    Fn = PROCEDURE (x: INTEGER): INTEGER;
    Rec = RECORD f: Fn END;

  VAR i, k: INTEGER; ch: CHAR; mant: REAL; e: INTEGER;
      b: Base; d: Derived; n: NodePtr; rc: Rec; v: ARRAY 3 OF INTEGER;

  PROCEDURE twice(x: INTEGER): INTEGER;
  BEGIN RETURN x * 2 END twice;

  PROCEDURE inner(a: ARRAY OF INTEGER): INTEGER;
    VAR i, t: INTEGER;
  BEGIN
    t := 0;
    FOR i := 0 TO LEN(a) - 1 DO t := t + a[i] END;
    RETURN t
  END inner;

  PROCEDURE forward(a: ARRAY OF INTEGER): INTEGER;
  BEGIN RETURN inner(a) END forward;

  PROCEDURE isDerived(VAR x: Base): INTEGER;
  BEGIN RETURN ORD(x IS Derived) END isDerived;

  PROCEDURE sliced(): INTEGER;
  BEGIN
    (* `b := d` must leave b's *own* dynamic type: a base variable may not
       advertise a derived one, which a VAR record parameter would see
       (DESIGN 9.3). *)
    b := d;
    RETURN isDerived(b)
  END sliced;

  PROCEDURE localRecord(): INTEGER;
    VAR loc: Derived;

    PROCEDURE which(VAR x: Base): INTEGER;
      VAR out: INTEGER;
    BEGIN
      CASE x OF
        Derived: out := 9
      | Base: out := 1
      | : out := 0
      END;
      RETURN out
    END which;

  BEGIN
    loc.a := 5; loc.b := 6;
    RETURN which(loc)
  END localRecord;

  PROCEDURE Run*;
  BEGIN
    (* integer `/` agrees with the folder's DIV *)
    Out.WriteInt((0 - 7) / 3, W); Out.WriteInt(7 / (0 - 3), W); Out.WriteLn;
    (* CHAR ordering uses ordinals, whatever C's `char` is *)
    Out.WriteInt(ORD(80X > 7FX), W); Out.WriteInt(ORD(41X < 7AX), W); Out.WriteLn;
    (* constants come from the checker's folding *)
    Out.WriteInt(Bc, W); Out.WriteInt(C1, W); Out.WriteLn;
    (* a CASE with a range and a multi-label arm *)
    i := 2; CASE i OF 1..3: k := 1 | 4, 5: k := 2 | 9: k := 3 END; Out.WriteInt(k, W);
    i := 5; CASE i OF 1..3: k := 1 | 4, 5: k := 2 | 9: k := 3 END; Out.WriteInt(k, W);
    i := 9; CASE i OF 1..3: k := 1 | 4, 5: k := 2 | 9: k := 3 END; Out.WriteInt(k, W);
    Out.WriteLn;
    (* record slicing keeps the destination's descriptor *)
    d.a := 1; d.b := 2;
    Out.WriteInt(sliced(), W); Out.WriteInt(b.a, W); Out.WriteLn;
    (* a local record as a VAR record parameter, with a type CASE *)
    Out.WriteInt(localRecord(), W); Out.WriteLn;
    (* forwarding an open array *)
    v[0] := 1; v[1] := 2; v[2] := 3;
    Out.WriteInt(forward(v), W); Out.WriteLn;
    (* calling a procedure value held in a record field *)
    rc.f := twice;
    Out.WriteInt(rc.f(21), W); Out.WriteLn;
    (* PACK scales by a value exponent; UNPK produces both results *)
    mant := 3.0; e := 0; PACK(mant, 1);
    Out.WriteInt(FLOOR(mant), W);
    UNPK(mant, e);
    Out.WriteInt(e, W); Out.WriteInt(FLOOR(mant * 10.0), W); Out.WriteLn;
    (* NEW on a recursive type, and a type test through a pointer *)
    NEW(n); n.key := 7;
    Out.WriteInt(n.key, W); Out.WriteInt(ORD(n IS NodePtr), W); Out.WriteLn;
    (* CHAR constants and a set built at run time *)
    ch := "A";
    Out.WriteInt(ORD(ch), W); Out.WriteInt(ORD(ch <= "B"), W); Out.WriteLn
  END Run;
BEGIN
END Regress.
