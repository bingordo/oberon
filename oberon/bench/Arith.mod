MODULE Arith;
  VAR i, j: INTEGER;
      acc*: INTEGER;
BEGIN
  acc := 1; j := 0;
  WHILE j < 2000 DO
    i := 0;
    WHILE i < 10000 DO acc := (acc * 1103515245 + 12345) MOD 1000003; i := i + 1 END;
    j := j + 1
  END
END Arith.
