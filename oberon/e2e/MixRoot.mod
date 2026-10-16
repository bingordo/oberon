MODULE MixRoot;
  (* Built by the Cranelift back end. It calls into `MixLib`, which is C. *)
  IMPORT MixLib;
  VAR i: INTEGER; q: MixLib.Pair;
BEGIN
  MixLib.Take(21); i := MixLib.Twice(21);
  ASSERT(i = 42); ASSERT(MixLib.shown = 21);
  (* A record this back end lays out, read field by field by the C one. *)
  q.x := 20; q.y := 22; ASSERT(MixLib.Sum(q) = 42)
END MixRoot.
