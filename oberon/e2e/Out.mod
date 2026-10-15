MODULE Out;
  IMPORT Texts, Oberon;
  VAR W: Texts.Writer;

  PROCEDURE Write*(ch: CHAR);
  BEGIN Texts.Write(W, ch) END Write;

  PROCEDURE WriteInt*(x, n: INTEGER);
    VAR i: INTEGER; a: ARRAY 16 OF CHAR;
  BEGIN
    i := 0;
    IF x < 0 THEN Texts.Write(W, "-"); x := -x END;
    REPEAT
      a[i] := CHR(x MOD 10 + ORD("0")); x := x DIV 10; INC(i)
    UNTIL x = 0;
    REPEAT Texts.Write(W, " "); DEC(n) UNTIL n <= i;
    REPEAT DEC(i); Texts.Write(W, a[i]) UNTIL i = 0
  END WriteInt;

  PROCEDURE WriteLn*;
  BEGIN Texts.WriteLn(W); Texts.Append(Oberon.Log, W.buf) END WriteLn;

BEGIN Texts.OpenWriter(W)
END Out.
