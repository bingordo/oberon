MODULE MixLeaf;
  (* Built by the Cranelift back end, called by a module the C back end built. *)
  TYPE Pair* = RECORD x*, y*: INTEGER END;
  VAR k*: INTEGER;
  PROCEDURE Sum*(VAR p: Pair): INTEGER;
  BEGIN RETURN p.x + p.y
  END Sum;
  PROCEDURE Quad*(x: INTEGER): INTEGER;
  BEGIN k := x * 4; RETURN k
  END Quad;
END MixLeaf.
