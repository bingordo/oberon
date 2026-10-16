MODULE CallArray;
  CONST N = 200000; R = 400;
  VAR a: ARRAY N OF INTEGER;
      i, j, sum: INTEGER;
      acc*: INTEGER;

  PROCEDURE Step(x: INTEGER): INTEGER;
  BEGIN RETURN x * 3 + 1
  END Step;

BEGIN
  i := 0;
  WHILE i < N DO a[i] := i MOD 7; i := i + 1 END;
  acc := 1; j := 0;
  WHILE j < R DO
    sum := 0; i := 0;
    WHILE i < N DO sum := sum + Step(a[i]); i := i + 1 END;
    acc := (acc + sum) MOD 1000003;
    j := j + 1
  END;
  ASSERT(acc > 0)
END CallArray.
