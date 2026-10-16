MODULE MixC2;
  (* Built by the C back end; its callee `MixLeaf` is a Cranelift object. *)
  IMPORT MixLeaf;
  VAR j: INTEGER; r: MixLeaf.Pair;
BEGIN
  j := MixLeaf.Quad(21); ASSERT(j = 84); ASSERT(MixLeaf.k = 84);
  (* And the other way: a record the C back end lays out, summed by the clif one. *)
  r.x := 19; r.y := 23; ASSERT(MixLeaf.Sum(r) = 42)
END MixC2.
