MODULE MixRoot;
  (* Built by the Cranelift back end. It calls into `MixLib`, which is C. *)
  IMPORT MixLib;
  VAR i: INTEGER;
BEGIN
  MixLib.Take(21); i := MixLib.Twice(21);
  ASSERT(i = 42); ASSERT(MixLib.shown = 21)
END MixRoot.
