MODULE NQueens;
  (* A standalone backtracking example: try every column in row order, prune a
     partial board as soon as a queen attacks one already placed, count all full
     boards. The classic eight-queens problem has 92 solutions. *)
  IMPORT Out;
  CONST N = 8;
  VAR column: ARRAY N OF INTEGER; solutions: INTEGER;

  PROCEDURE Search(row: INTEGER);
    VAR c, r: INTEGER; safe: BOOLEAN;
  BEGIN
    IF row = N THEN
      INC(solutions)
    ELSE
      FOR c := 0 TO N - 1 DO
        safe := TRUE; r := 0;
        WHILE (r < row) & safe DO
          IF (column[r] = c) OR (ABS(column[r] - c) = row - r) THEN
            safe := FALSE
          END;
          INC(r)
        END;
        IF safe THEN
          column[row] := c;
          Search(row + 1)
        END
      END
    END
  END Search;

BEGIN
  Search(0);
  ASSERT(solutions = 92);
  Out.Int(solutions, 0); Out.Ln
END NQueens.
