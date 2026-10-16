MODULE MixC2;
  (* Built by the C back end; its callee `MixLeaf` is a Cranelift object. *)
  IMPORT MixLeaf;
  VAR j: INTEGER;
BEGIN
  j := MixLeaf.Quad(21); ASSERT(j = 84); ASSERT(MixLeaf.k = 84)
END MixC2.
