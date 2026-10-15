(* report §11.1's SYSTEM, every entry of its table, checked against a
   hand-computed expectation: an ADR/PUT/GET round trip, BIT on a word written
   that way, SIZE against the layout, and COPY between two arrays. Each ASSERT
   names the rule it tests, so a failure says which one broke -- and one
   procedure takes the address of *locals*, which is the case a 32-bit integer
   address cannot carry (DESIGN R31). *)
MODULE Sys;
  IMPORT SYSTEM;
  TYPE R = RECORD a, b: INTEGER END; P = POINTER TO R;
       Arr4 = ARRAY 4 OF INTEGER; Three = ARRAY 3 OF CHAR;
  VAR w, got: INTEGER;
      cbuf: ARRAY 4 OF CHAR; ch: CHAR;
      f, f2: BOOLEAN; r, r2: REAL;
      src: ARRAY 2 OF INTEGER; big: ARRAY 4 OF INTEGER;
      srcR: R; pair: ARRAY 2 OF R; p: P;

  (* A local variable lives on the stack, whose address does not fit in an
     INTEGER: ADR must still hand out an address GET and PUT can use. *)
  PROCEDURE Locals;
    VAR v, got: INTEGER; c: CHAR; b: BOOLEAN; x, y: REAL;
  BEGIN
    v := 12345;
    SYSTEM.PUT(SYSTEM.ADR(v), 987654);
    ASSERT(v = 987654);
    SYSTEM.GET(SYSTEM.ADR(v), got);
    ASSERT(got = 987654);
    ASSERT(SYSTEM.ADR(v) # SYSTEM.ADR(got));

    c := "A";
    SYSTEM.PUT(SYSTEM.ADR(c), ORD(c));
    ASSERT(ORD(c) = 65);
    SYSTEM.GET(SYSTEM.ADR(c), c);
    ASSERT(c = "A");

    b := FALSE;
    SYSTEM.PUT(SYSTEM.ADR(b), TRUE);
    ASSERT(b);
    SYSTEM.GET(SYSTEM.ADR(b), b);
    ASSERT(b);

    x := 2.5; y := 0.0;
    SYSTEM.PUT(SYSTEM.ADR(x), x);
    SYSTEM.GET(SYSTEM.ADR(x), y);
    ASSERT(y = 2.5)
  END Locals;

BEGIN
  Locals;

  (* ADR: the address of a variable is the one GET/PUT round-trip through. *)
  w := 12345;
  SYSTEM.PUT(SYSTEM.ADR(w), 987654);
  ASSERT(w = 987654);
  SYSTEM.GET(SYSTEM.ADR(w), got);
  ASSERT(got = 987654);

  (* BIT: bit n of the word the address points at. *)
  SYSTEM.PUT(SYSTEM.ADR(w), 8);
  ASSERT(SYSTEM.BIT(SYSTEM.ADR(w), 3));
  ASSERT(~SYSTEM.BIT(SYSTEM.ADR(w), 2));
  SYSTEM.PUT(SYSTEM.ADR(w), -1);
  ASSERT(SYSTEM.BIT(SYSTEM.ADR(w), 0));
  ASSERT(SYSTEM.BIT(SYSTEM.ADR(w), 31));
  SYSTEM.PUT(SYSTEM.ADR(w), 0);
  ASSERT(~SYSTEM.BIT(SYSTEM.ADR(w), 31));

  (* PUT stores the word its *value* occupies: an INTEGER fills it ... *)
  cbuf[0] := "A"; cbuf[1] := "B"; cbuf[2] := "C"; cbuf[3] := "D";
  SYSTEM.PUT(SYSTEM.ADR(cbuf[0]), 65535);
  ASSERT(ORD(cbuf[0]) = 255);
  ASSERT(ORD(cbuf[1]) = 255);
  ASSERT(ORD(cbuf[2]) = 0);
  ASSERT(ORD(cbuf[3]) = 0);
  (* ... and a CHAR contributes its low byte and zeroes the rest of the word. *)
  cbuf[1] := "B";
  ch := CHR(255);
  SYSTEM.PUT(SYSTEM.ADR(cbuf[0]), ch);
  ASSERT(ORD(cbuf[0]) = 255);
  ASSERT(ORD(cbuf[1]) = 0);
  (* GET narrows the word it reads to the variable's own type. *)
  SYSTEM.PUT(SYSTEM.ADR(w), -1);
  SYSTEM.GET(SYSTEM.ADR(w), ch);
  ASSERT(ORD(ch) = 255);
  SYSTEM.PUT(SYSTEM.ADR(w), 0);
  SYSTEM.GET(SYSTEM.ADR(w), ch);
  ASSERT(ORD(ch) = 0);

  f2 := FALSE;
  SYSTEM.PUT(SYSTEM.ADR(f2), TRUE);
  ASSERT(f2);
  SYSTEM.GET(SYSTEM.ADR(f2), f);
  ASSERT(f);
  SYSTEM.PUT(SYSTEM.ADR(f2), FALSE);
  ASSERT(~f2);

  r := 2.5; r2 := 0.0;
  SYSTEM.PUT(SYSTEM.ADR(r), r);
  SYSTEM.GET(SYSTEM.ADR(r), r2);
  ASSERT(r2 = 2.5);

  (* SIZE: the extent the layout gives, in bytes. *)
  ASSERT(SYSTEM.SIZE(INTEGER) = 4);
  ASSERT(SYSTEM.SIZE(CHAR) = 1);
  ASSERT(SYSTEM.SIZE(REAL) = 4);
  ASSERT(SYSTEM.SIZE(BOOLEAN) = 4);
  ASSERT(SYSTEM.SIZE(SET) = 4);
  ASSERT(SYSTEM.SIZE(Arr4) = 4 * SYSTEM.SIZE(INTEGER));
  ASSERT(SYSTEM.SIZE(Three) = 3);
  (* The stride between two adjacent objects of a type *is* the padded extent the
     descriptor is built beside, so a record whose descriptor sat outside its
     Oberon fields would fail here. *)
  ASSERT(SYSTEM.ADR(pair[1]) - SYSTEM.ADR(pair[0]) = SYSTEM.SIZE(R));

  (* COPY: n words, four bytes each, and no word outside them. *)
  big[0] := 7; big[1] := -1; big[2] := -1; big[3] := 9;
  src[0] := 10000H; src[1] := 2;
  SYSTEM.COPY(SYSTEM.ADR(src[0]), SYSTEM.ADR(big[1]), 2);
  ASSERT(big[0] = 7);
  ASSERT(big[1] = 10000H);
  ASSERT(big[2] = 2);
  ASSERT(big[3] = 9);

  (* A whole record, addressed through a pointer target, moved by SIZE. *)
  srcR.a := 11; srcR.b := 22;
  NEW(p);
  SYSTEM.COPY(SYSTEM.ADR(srcR), SYSTEM.ADR(p^), SYSTEM.SIZE(R) DIV 4);
  ASSERT(p IS R);
  ASSERT(p.a = 11);
  ASSERT(p.b = 22)
END Sys.
