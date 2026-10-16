MODULE RandomNumbers;
  (* Adapted from https://github.com/lboasso/oberonc, examples/fern/RandomNumbers.Mod.
     MIT License, Copyright (c) 2017 Luca Boasso. The unused Exp procedure and
     Math import are omitted; Uniform and InitSeed retain the source algorithm. *)
  VAR Z: INTEGER;

  PROCEDURE Uniform*(): REAL;
    VAR g: INTEGER;
  BEGIN
    (* Park-Miller's Schrage form: q = 2147483647 DIV 16807 and
       r = 2147483647 MOD 16807, chosen to keep intermediate integers in range. *)
    g := 16807 * (Z MOD 127773) - 2836 * (Z DIV 127773);
    IF g > 0 THEN Z := g ELSE Z := g + 2147483647 END;
    RETURN FLT(Z) * (1.0 / FLT(2147483647))
  END Uniform;

  PROCEDURE InitSeed*(seed: INTEGER);
  BEGIN
    Z := seed
  END InitSeed;

BEGIN
  Z := 1
END RandomNumbers.
