MODULE Trap;
  IMPORT Out;
  CONST W = 8;
  VAR a: ARRAY 4 OF INTEGER;
  PROCEDURE Run*;
    VAR i: INTEGER;
  BEGIN
    FOR i := 0 TO 3 DO a[i] := i END;
    (* 10 is out of range for `a`: the check must trap and say where. *)
    Out.WriteInt(a[10], W)
  END Run;
BEGIN
END Trap.
