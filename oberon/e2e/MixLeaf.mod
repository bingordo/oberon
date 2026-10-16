MODULE MixLeaf;
  (* Built by the Cranelift back end, called by a module the C back end built. *)
  VAR k*: INTEGER;
  PROCEDURE Quad*(x: INTEGER): INTEGER;
  BEGIN k := x * 4; RETURN k
  END Quad;
END MixLeaf.
