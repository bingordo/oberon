MODULE Out;
  (* The module Oberon-07 programs written elsewhere expect: `Int`, `Char`,
     `String` and `Ln` over the report's `Texts`. It is not part of the report.
     `Out` is a convention of the Oberon implementations, and a program that
     prints reaches for it. It lives here rather than being something the
     compiler knows about, exactly as the host's arguments do through `Args`
     (DESIGN 11). This file and the `Args` interface the driver writes are the two
     halves: the compiler ships no library, and a program that wants one puts this
     directory on the search path.

     `e2e/Out.mod` is a different module with the same name, written with the
     names that suite's programs use; a build that wants this one puts this
     directory first. *)
  IMPORT Texts, Oberon;
  VAR W: Texts.Writer;

  PROCEDURE Open*;
  BEGIN Texts.OpenWriter(W)
  END Open;

  PROCEDURE Char*(ch: CHAR);
  BEGIN Texts.Write(W, ch)
  END Char;

  PROCEDURE String*(s: ARRAY OF CHAR);
  BEGIN Texts.WriteString(W, s)
  END String;

  (* Right-aligned in `n` columns, as the implementations do it: the digits are
     produced least significant first and then written backwards, because the
     width is only known once they are counted. `Texts` here has no `WriteInt`
     (the report's has, and this is the shape that works without it). *)
  PROCEDURE Int*(x, n: INTEGER);
    VAR i: INTEGER; a: ARRAY 16 OF CHAR;
  BEGIN
    i := 0;
    IF x < 0 THEN Texts.Write(W, "-"); x := -x END;
    REPEAT
      a[i] := CHR(x MOD 10 + ORD("0")); x := x DIV 10; INC(i)
    UNTIL x = 0;
    REPEAT Texts.Write(W, " "); DEC(n) UNTIL n <= i;
    REPEAT DEC(i); Texts.Write(W, a[i]) UNTIL i = 0
  END Int;

  PROCEDURE Ln*;
  BEGIN Texts.WriteLn(W); Texts.Append(Oberon.Log, W.buf)
  END Ln;

BEGIN Texts.OpenWriter(W)
END Out.
