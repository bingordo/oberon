MODULE InlVar;
  (* The order hazard rather than the set one: both functions declare both of the
     module's variables, but a map's iteration order is not the same in two
     functions, and a spliced `GlobalValue` keeps the callee's name index. The callee
     touches `b` and the body touches both, so if the two orders disagreed the
     assertions would read the wrong variable. *)
  VAR a, b: INTEGER;
      got*: INTEGER;
  PROCEDURE Bump;
  BEGIN b := b + 1; got := b
  END Bump;
BEGIN
  a := 10; b := 20; Bump;
  ASSERT(b = 21); ASSERT(a = 10); ASSERT(got = 21)
END InlVar.
