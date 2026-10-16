MODULE InlDesc;
  (* The same rule as InlStr, for the other data a function declares as it uses it:
     the callee's guard needs `Ext`'s record descriptor and the module body never
     mentions it. A spliced descriptor reference that resolved to the caller's name
     table would read the wrong descriptor, and `got` would say so. *)
  TYPE Base* = RECORD x*: INTEGER END;
       Ext* = RECORD (Base) y*: INTEGER END;
  VAR b: Base; got*: INTEGER;
  PROCEDURE Guard*(VAR v: Base);
  BEGIN
    IF v IS Ext THEN got := 1 ELSE got := 2 END
  END Guard;
BEGIN
  b.x := 5; Guard(b); ASSERT(got = 2)
END InlDesc.
