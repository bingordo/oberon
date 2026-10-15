MODULE Hello;
  IMPORT Texts;
  VAR W: Texts.Writer;

  PROCEDURE Greet*;
  BEGIN Texts.WriteString(W, "Hello again from a command") END Greet;

BEGIN
  Texts.OpenWriter(W);
  Texts.WriteString(W, "Hello, world!");
  Texts.WriteLn(W);
  Greet;
  Texts.WriteLn(W)
END Hello.
