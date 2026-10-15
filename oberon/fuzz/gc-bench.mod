MODULE GcBench;
  TYPE N = RECORD next: POINTER TO N; k: INTEGER END; P = POINTER TO N;
  VAR head, p, q: P; i, n: INTEGER; ok: ARRAY 1 OF INTEGER;
BEGIN
  head := NIL;
  FOR i := 0 TO 4999 DO NEW(p); p.k := i; p.next := head; head := p END;
  FOR i := 0 TO 200000 DO NEW(q); q.k := 7 END;      (* churn: collected, not kept *)
  n := 0; p := head;
  WHILE p # NIL DO n := n + 1; p := p.next END;
  IF n # 5000 THEN ok[1] := 0 END
END GcBench.
