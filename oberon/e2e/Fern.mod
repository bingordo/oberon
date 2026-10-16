MODULE Fern;
  (* The Barnsley fern's iterated-function-system example from
     https://github.com/lboasso/oberonc/tree/master/examples/fern (MIT License,
     Copyright (c) 2017 Luca Boasso), adapted from XYplane.Dot to headless output.
     The four affine transforms, their probabilities, initial point, seed, and
     100,000 iterations are retained; output reports transform counts, pixel bounds,
     and a rolling checksum instead of drawing into the source's graphics window. *)
  IMPORT Out, RandomNumbers;
  VAR X, Y: REAL;
      count1, count2, count3, count4: INTEGER;
      i, px, py, minX, maxX, minY, maxY, checksum: INTEGER;
      rn, nx, ny: REAL;

  PROCEDURE Draw;
  BEGIN
    i := 0;
    minX := 2147483647; maxX := -2147483647;
    minY := 2147483647; maxY := -2147483647;
    checksum := 0;
    count1 := 0; count2 := 0; count3 := 0; count4 := 0;
    REPEAT
      rn := RandomNumbers.Uniform();
      IF rn < 0.01 THEN
        nx := 0.0 * X + 0.0 * Y;
        ny := 0.0 * X + 0.16 * Y;
        INC(count1)
      ELSIF rn < 0.86 THEN
        nx := 0.85 * X + 0.04 * Y;
        ny := -0.04 * X + 0.85 * Y + 1.6;
        INC(count2)
      ELSIF rn < 0.93 THEN
        nx := 0.2 * X - 0.26 * Y;
        ny := 0.23 * X + 0.22 * Y + 1.6;
        INC(count3)
      ELSE
        nx := -0.15 * X + 0.28 * Y;
        ny := 0.26 * X + 0.24 * Y + 0.44;
        INC(count4)
      END;
      X := nx; Y := ny;
      px := 320 + FLOOR(X * 64.0);
      py := 75 + FLOOR(Y * 64.0);
      IF px < minX THEN minX := px END;
      IF px > maxX THEN maxX := px END;
      IF py < minY THEN minY := py END;
      IF py > maxY THEN maxY := py END;
      checksum := (checksum + px * (i MOD 97) + py) MOD 1000003;
      INC(i)
    UNTIL i = 100000
  END Draw;

BEGIN
  X := 0.0; Y := 0.0;
  RandomNumbers.InitSeed(1);
  Draw;
  Out.Int(i, 0); Out.String(" ");
  Out.Int(count1, 0); Out.String(" ");
  Out.Int(count2, 0); Out.String(" ");
  Out.Int(count3, 0); Out.String(" ");
  Out.Int(count4, 0); Out.String(" ");
  Out.Int(minX, 0); Out.String(" "); Out.Int(maxX, 0); Out.String(" ");
  Out.Int(minY, 0); Out.String(" "); Out.Int(maxY, 0); Out.String(" ");
  Out.Int(checksum, 0); Out.Ln
END Fern.
