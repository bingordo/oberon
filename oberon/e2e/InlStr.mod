MODULE InlStr;
  (* An inlined callee whose body names a string literal, and a module body that
     names a different one. This is the shape that first made inlining output the
     wrong string: the spliced literal resolved to the caller's data, because a
     `GlobalValueData` is copied into the caller verbatim and its `User` name is an
     index into the *function's* name table (PLAN.md W9.4). One character is enough
     to tell "FIRST" from "SECOND", so the assertion is the whole check. *)
  VAR s*: ARRAY 16 OF CHAR;
  PROCEDURE Set;
  BEGIN s := "SECOND"
  END Set;
BEGIN
  s := "FIRST"; Set; ASSERT(s[1] = "E")
END InlStr.
