MODULE MixLib;
  (* Built by the C back end and linked into a program whose other modules come
     from the second back end. The second's assertions are the check. *)
  TYPE Pair* = RECORD x*, y*: INTEGER END;
  VAR shown*: INTEGER;
  PROCEDURE Sum*(VAR p: Pair): INTEGER;
  BEGIN RETURN p.x + p.y
  END Sum;
  PROCEDURE Twice*(x: INTEGER): INTEGER;
  BEGIN RETURN x * 2
  END Twice;
  PROCEDURE Take*(v: INTEGER);
  BEGIN ASSERT(v = 21); shown := v
  END Take;
END MixLib.
