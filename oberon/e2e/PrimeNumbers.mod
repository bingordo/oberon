MODULE PrimeNumbers;  (* Tabulate prime numbers *)
  (* From https://github.com/lboasso/oberonc (examples/PrimeNumbers.Mod), MIT
     License, Copyright (c) 2017 Luca Boasso.

     Ported, and the port is the point: that compiler provides `ARGNUM` and `ARGS`
     as built-ins, which are neither the report's nor portable. Here the host's
     arguments come from the `Args` module instead (DESIGN 11), so the only
     changes are the import, `Args.Count` for `ARGNUM()`, `Args.Get` for `ARGS`,
    and a module body that calls `Main`. That compiler calls it for you. *)
  IMPORT Args, Out, Util;

  PROCEDURE Primes(n: INTEGER);
    VAR i, k, m, x, inc, lim, sqr: INTEGER;
      prim: BOOLEAN;
      p: ARRAY 400 OF INTEGER;
      v: ARRAY 20 OF INTEGER;
  BEGIN
    x := 1; inc := 4;
    lim := 1; sqr := 4;
     m := 0; i := 3;
    WHILE i <= n DO
      REPEAT x := x + inc; inc := 6 - inc;
        IF sqr <= x THEN  (*sqr = p[lim]^2*)
          v[lim] := sqr; lim := lim + 1; sqr := p[lim]*p[lim]
        END ;
        k := 2; prim := TRUE;
        WHILE prim & (k < lim) DO
          k := k+1;
          IF v[k] < x THEN v[k] := v[k] + p[k] END ;
          prim := x # v[k]
        END
      UNTIL prim;
      p[i] := x; Out.Int(x, 5);
      IF m = 10 THEN Out.Ln; m := 0 ELSE m := m+1 END ;
      i := i+1
    END ;
    IF m > 0 THEN Out.Ln END
  END Primes;

  PROCEDURE Main;
    VAR n: INTEGER;
      arg: ARRAY 3 OF CHAR;
  BEGIN
    n := Args.Count();
    IF n # 1 THEN
      Out.String("usage: PrimeNumbers number in [0..99]");
      Out.Ln
    ELSE
      Args.Get(0, arg);
      Primes(Util.strToInt(arg))
    END
  END Main;

BEGIN Main
END PrimeNumbers.
