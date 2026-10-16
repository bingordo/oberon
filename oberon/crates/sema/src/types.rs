//! The Oberon-07 type algebra, executed.
//!
//! This module is the part of "type checking and semantic checking" that the
//! design states as *rules* and that is worth pinning down with tests before any
//! resolver feeds it: type identity, the extension relation, assignment
//! compatibility (report section 9.1), parameter matching (report section 10.1), the operand
//! rules of the operators (report section 8.2) and the signatures of the predeclared
//! procedures (report section 10.2).
//!
//! The type module is independent of the AST and accepts `TypeId`s. These rules
//! serve the checker and constant folder. The back end uses them for layout.
//!
//! Nominal versus structural, and why (report section 6):
//!
//! * **records** are nominal. Report section 6.3 defines extension ("T extends T0 if it
//!   equals T0, or if it directly extends an extension of T0") and section 6.3's whole
//!   point is that a *declaration* creates a type; two records with identical
//!   fields written twice are two types.
//! * **pointers** are equal when their base types are equal. The report defines
//!   pointer types only through the base (section 6.4: "P is said to be bound to T",
//!   "Pointer types inherit the extension relation of their base types"), so
//!   `POINTER TO T` written twice denotes the same type, and `P` extends `P0`
//!   exactly when its base extends `P0`'s base.
//! * **arrays** and **procedure types** are structural: sections 9.1.4 and 10.1 compare
//!   their components, and nothing in the report gives an array declaration an
//!   identity beyond its shape.
//! * **basic types** are their names. The report treats `BYTE` and `INTEGER` as
//!   *compatible* in both directions (section 6.1), but they are not the same type.

use std::collections::HashMap;

use syntax::ast::BinOp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypeId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Basic {
    Boolean,
    Char,
    Integer,
    Real,
    Byte,
    Set,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Param {
    /// `VAR` parameter (report section 10.1).
    pub by_var: bool,
    pub ty: TypeId,
}

/// A record field has a name and type. It also records whether the declaration has
/// the report's public mark (report section 6.3). The mark travels through an interface so a
/// client can tell which fields it may name (DESIGN section 7).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Field {
    pub name: String,
    pub ty: TypeId,
    pub export: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    Basic(Basic),
    /// `ARRAY len OF elem`
    Array { len: u32, elem: TypeId },
    /// `ARRAY OF elem` - only legal as a formal type (report section 10.1).
    OpenArray { elem: TypeId },
    /// Nominal type. `def` identifies its declaration.
    Record {
        def: u32,
        base: Option<TypeId>,
        fields: Vec<Field>,
    },
    Pointer { base: TypeId },
    Proc {
        params: Vec<Param>,
        result: Option<TypeId>,
    },
    /// The type of `NIL` (report section 6.4).
    Nil,
    /// The type of a string literal: its length counts (report section 9.1.2).
    StringLit(usize),
    /// Poison, used after reporting an error so one mistake does not cascade.
    Error,
}

/// Hash-consing key. Only the structural kinds are interned; records are not,
/// because their identity is their declaration.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    Basic(Basic),
    Array(u32, TypeId),
    OpenArray(TypeId),
    Proc(Vec<Param>, Option<TypeId>),
    Nil,
    StringLit(usize),
    Error,
}

#[derive(Debug, Default)]
pub struct Types {
    types: Vec<Type>,
    structural: HashMap<Key, TypeId>,
    next_def: u32,
}

impl Types {
    pub fn new() -> Types {
        let mut t = Types::default();
        // Preintern the basics so `ty_bool()` and friends are lookups.
        for b in [
            Basic::Boolean,
            Basic::Char,
            Basic::Integer,
            Basic::Real,
            Basic::Byte,
            Basic::Set,
        ] {
            t.intern_key(Key::Basic(b), Type::Basic(b));
        }
        t.intern_key(Key::Nil, Type::Nil);
        t.intern_key(Key::Error, Type::Error);
        t
    }

    fn intern_key(&mut self, key: Key, ty: Type) -> TypeId {
        if let Some(id) = self.structural.get(&key) {
            return *id;
        }
        let id = TypeId(self.types.len() as u32);
        self.types.push(ty);
        self.structural.insert(key, id);
        id
    }

    pub fn get(&self, id: TypeId) -> &Type {
        &self.types[id.0 as usize]
    }

    /// Lookup of an interned key that `new()` guarantees to exist. These take
    /// `&self` so that a nested call such as `t.array(4, t.integer())` borrows
    /// the interner mutably once and reads it for the argument.
    fn preinterned(&self, key: &Key) -> TypeId {
        *self
            .structural
            .get(key)
            .expect("Types::new preinterns every basic type, NIL and ERROR")
    }
    pub fn basic(&self, b: Basic) -> TypeId {
        self.preinterned(&Key::Basic(b))
    }
    pub fn boolean(&self) -> TypeId {
        self.basic(Basic::Boolean)
    }
    pub fn char(&self) -> TypeId {
        self.basic(Basic::Char)
    }
    pub fn integer(&self) -> TypeId {
        self.basic(Basic::Integer)
    }
    pub fn real(&self) -> TypeId {
        self.basic(Basic::Real)
    }
    pub fn byte(&self) -> TypeId {
        self.basic(Basic::Byte)
    }
    pub fn set(&self) -> TypeId {
        self.basic(Basic::Set)
    }
    pub fn nil(&self) -> TypeId {
        self.preinterned(&Key::Nil)
    }
    pub fn error(&self) -> TypeId {
        self.preinterned(&Key::Error)
    }
    pub fn string_lit(&mut self, n: usize) -> TypeId {
        self.intern_key(Key::StringLit(n), Type::StringLit(n))
    }
    pub fn array(&mut self, len: u32, elem: TypeId) -> TypeId {
        self.intern_key(Key::Array(len, elem), Type::Array { len, elem })
    }
    pub fn open_array(&mut self, elem: TypeId) -> TypeId {
        self.intern_key(Key::OpenArray(elem), Type::OpenArray { elem })
    }
    pub fn proc(&mut self, params: Vec<Param>, result: Option<TypeId>) -> TypeId {
        self.intern_key(
            Key::Proc(params.clone(), result),
            Type::Proc { params, result },
        )
    }
    /// Each call to `record` creates a *different* type, as does each `TYPE`
    /// declaration (report section 6.3).
    pub fn record(&mut self, base: Option<TypeId>, fields: Vec<Field>) -> TypeId {
        let id = self.record_placeholder();
        self.define_record(id, base, fields);
        id
    }

    /// Allocate a record's *identity* before its fields have been declared. The report
    /// allows one forward reference inside a scope: a pointer's base type
    /// (report section 6.4: "the identifier T can be declared textually following
    /// the declaration of P"). The resolver needs the identity first and
    /// the fields later.
    pub fn record_placeholder(&mut self) -> TypeId {
        let def = self.next_def;
        self.next_def += 1;
        let id = TypeId(self.types.len() as u32);
        self.types.push(Type::Record {
            def,
            base: None,
            fields: Vec::new(),
        });
        id
    }

    /// Fill in a placeholder. Panics if `id` is not a record placeholder, which
    /// can only happen through a resolver bug.
    pub fn define_record(&mut self, id: TypeId, base: Option<TypeId>, fields: Vec<Field>) {
        match &mut self.types[id.0 as usize] {
            Type::Record {
                base: b,
                fields: f,
                ..
            } => {
                *b = base;
                *f = fields;
            }
            other => panic!("define_record on a non-record: {other:?}"),
        }
    }

    /// A pointer type is identified by its base (section 6.4), so this is
    /// interned by base rather than given a fresh identity.
    pub fn pointer(&mut self, base: TypeId) -> TypeId {
        let id = TypeId(self.types.len() as u32);
        // Pointers are equal by base, so intern on that: find an existing
        // pointer with the same base.
        for (i, t) in self.types.iter().enumerate() {
            if let Type::Pointer { base: b } = t {
                if *b == base {
                    return TypeId(i as u32);
                }
            }
        }
        self.types.push(Type::Pointer { base });
        id
    }

    // queries

    pub fn is_error(&self, t: TypeId) -> bool {
        matches!(self.get(t), Type::Error)
    }
    pub fn is_bool(&self, t: TypeId) -> bool {
        matches!(self.get(t), Type::Basic(Basic::Boolean))
    }
    pub fn is_char(&self, t: TypeId) -> bool {
        matches!(self.get(t), Type::Basic(Basic::Char))
    }
    /// `INTEGER` or `BYTE`: report section 6.1 makes them compatible, and section 8.2.2
    /// makes `DIV`/`MOD` apply to "integer operands", including `BYTE`.
    pub fn is_integer_like(&self, t: TypeId) -> bool {
        matches!(
            self.get(t),
            Type::Basic(Basic::Integer) | Type::Basic(Basic::Byte)
        )
    }
    pub fn is_numeric(&self, t: TypeId) -> bool {
        matches!(
            self.get(t),
            Type::Basic(Basic::Integer)
                | Type::Basic(Basic::Byte)
                | Type::Basic(Basic::Real)
        )
    }
    pub fn is_set(&self, t: TypeId) -> bool {
        matches!(self.get(t), Type::Basic(Basic::Set))
    }
    /// Report section 6.1 defines the basic types accepted by SYSTEM.GET and
    /// SYSTEM.PUT (their argument may be "any basic type"), but not records,
    /// arrays or pointers.
    pub fn is_basic(&self, t: TypeId) -> bool {
        matches!(self.get(t), Type::Basic(_))
    }
    pub fn is_pointer(&self, t: TypeId) -> bool {
        matches!(self.get(t), Type::Pointer { .. })
    }
    pub fn is_proc(&self, t: TypeId) -> bool {
        matches!(self.get(t), Type::Proc { .. })
    }
    pub fn is_record(&self, t: TypeId) -> bool {
        matches!(self.get(t), Type::Record { .. })
    }
    pub fn is_nil(&self, t: TypeId) -> bool {
        matches!(self.get(t), Type::Nil)
    }
    /// A `CHAR` value can also be a one-character string. The report's `WriteInt`
    /// procedure uses one where `CHAR` is expected (section 10.1). It compares
    /// `ch <= "9"` and calls `ORD("0")`.
    pub fn is_char_like(&self, t: TypeId) -> bool {
        self.is_char(t) || matches!(self.get(t), Type::StringLit(1))
    }
    /// A `CHAR` array is a string. Report section 3 defines strings as sequences of
    /// characters. Section 9.1.2 assigns them to "any array of characters". A string
    /// *literal* is one too, which is what makes the report's own
    /// `text < "abd"` a character-array relation.
    pub fn is_char_array(&self, t: TypeId) -> bool {
        match self.get(t) {
            Type::Array { elem, .. } | Type::OpenArray { elem } => self.is_char(*elem),
            Type::StringLit(_) => true,
            _ => false,
        }
    }
    pub fn array_len(&self, t: TypeId) -> Option<u32> {
        match self.get(t) {
            Type::Array { len, .. } => Some(*len),
            _ => None,
        }
    }
    pub fn elem(&self, t: TypeId) -> Option<TypeId> {
        match self.get(t) {
            Type::Array { elem, .. } | Type::OpenArray { elem } => Some(*elem),
            _ => None,
        }
    }
    /// The type a `^` selector produces, or the record a `.field` applies to
    /// (the report's `.` implies dereferencing, section 8.1).
    pub fn deref(&self, t: TypeId) -> Option<TypeId> {
        match self.get(t) {
            Type::Pointer { base } => Some(*base),
            _ => None,
        }
    }
    /// Field lookup searches the record and its base types. A more derived
    /// declaration wins. Report section 6.3 scopes field names to the record
    /// definition itself.
    /// A field's type *and* the record that declares it: for an inherited field
    /// that is the base's declaration, which is the one whose export mark counts
    /// (report section 6.3).
    pub fn field_decl(&self, mut t: TypeId, name: &str) -> Option<(TypeId, TypeId)> {
        let mut guard = 0;
        loop {
            guard += 1;
            if guard > self.types.len() {
                return None;
            }
            match self.get(t) {
                Type::Record { base, fields, .. } => {
                    if let Some(f) = fields.iter().find(|f| f.name == name) {
                        return Some((t, f.ty));
                    }
                    t = (*base)?;
                }
                _ => return None,
            }
        }
    }

    pub fn field(&self, mut t: TypeId, name: &str) -> Option<TypeId> {
        let mut guard = 0;
        loop {
            guard += 1;
            if guard > self.types.len() {
                return None;
            }
            match self.get(t) {
                Type::Record { base, fields, .. } => {
                    if let Some(f) = fields.iter().find(|f| f.name == name) {
                        return Some(f.ty);
                    }
                    t = (*base)?;
                }
                _ => return None,
            }
        }
    }

    // relations

    /// `same_type` matches the report's "is the same type" (used by sections 6.2 and 10.1).
    pub fn same_type(&self, a: TypeId, b: TypeId) -> bool {
        if a == b {
            // Covers the basics, the hash-consed structural kinds, NIL and
            // ERROR. Records and pointers are never hash-consed to a shared id.
            return true;
        }
        match (self.get(a), self.get(b)) {
            (Type::Record { def: x, .. }, Type::Record { def: y, .. }) => x == y,
            (Type::Pointer { base: x }, Type::Pointer { base: y }) => self.same_type(*x, *y),
            (Type::StringLit(x), Type::StringLit(y)) => x == y,
            (Type::Basic(x), Type::Basic(y)) => x == y,
            _ => false,
        }
    }

    /// `t` extends `base` (report sections 6.3 and 6.4), which is reflexive.
    pub fn extends(&self, t: TypeId, base: TypeId) -> bool {
        let mut cur = t;
        let mut guard = 0;
        loop {
            guard += 1;
            if guard > self.types.len() {
                return false;
            }
            if self.same_type(cur, base) {
                return true;
            }
            match self.get(cur) {
                Type::Record { base: Some(b), .. } => cur = *b,
                // Report section 6.4: pointer types inherit the extension relation
                // of their base types.
                Type::Pointer { base: pb } => match self.get(base) {
                    Type::Pointer { base: qb } => {
                        if !self.extends(*pb, *qb) {
                            return false;
                        }
                        return true;
                    }
                    _ => return false,
                },
                _ => return false,
            }
        }
    }

    /// The report's `T0` ranges over `v IS T0`, guards and type `CASE` labels:
    /// the tested type must be an extension of the declared type, and the
    /// designator must be a pointer or a `VAR` record parameter (sections 8.1 and 8.2.4).
    pub fn test_applicable(&self, declared: TypeId, tested: TypeId) -> bool {
        let designator_ok = self.is_pointer(declared)
            || self.is_record(declared) // the VAR-record-parameter half is the checker's
            || self.is_error(declared)
            || self.is_error(tested);
        designator_ok && self.extends(tested, declared)
    }

    // assignments

    /// Report section 9.1 has four exceptions, plus the `BYTE`/`INTEGER`
    /// compatibility of section 6.1.
    pub fn is_assignable(&self, dst: TypeId, src: TypeId) -> bool {
        if self.is_error(dst) || self.is_error(src) {
            return true; // already reported
        }
        if self.same_type(dst, src) {
            return true;
        }
        // Report section 6.1: BYTE is compatible with INTEGER, and vice-versa.
        if self.is_integer_like(dst) && self.is_integer_like(src) {
            return true;
        }
        match self.get(src) {
            // Section 9.1.1: NIL may be assigned to any pointer or procedure variable.
            Type::Nil => self.is_pointer(dst) || self.is_proc(dst),
            // Section 9.1.2: a string may be assigned to any array of characters if it
            // is shorter than the array (a NUL is appended), and a
            // one-character string to a CHAR.
            Type::StringLit(n) => match self.get(dst) {
                Type::Array { len, elem } if self.is_char(*elem) => (*n as u32) < *len,
                Type::OpenArray { elem } => self.is_char(*elem),
                Type::Basic(Basic::Char) => *n == 1,
                _ => false,
            },
            // Section 9.1.3: the source must be an extension of the destination.
            // Records and pointers follow this relation. Section 6.4 says that pointer
            // types inherit the extension relation of their base types.
            Type::Record { .. } => self.is_record(dst) && self.extends(src, dst),
            Type::Pointer { .. } => self.is_pointer(dst) && self.extends(src, dst),
            // Section 9.1.4: an open array may be assigned to an array of equal base
            // type.
            Type::OpenArray { elem } => match self.get(dst) {
                Type::Array { elem: de, .. } | Type::OpenArray { elem: de } => {
                    self.same_type(*elem, *de)
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// Report section 10.1. `actual` is the actual parameter's type; whether a `VAR`
    /// actual is a *variable* is the checker's business, not this predicate's.
    pub fn param_ok(&self, formal: &Param, actual: TypeId) -> bool {
        if self.is_error(formal.ty) || self.is_error(actual) {
            return true;
        }
        // An open array formal accepts any length, but not any element type.
        if let Type::OpenArray { elem } = self.get(formal.ty) {
            return match self.get(actual) {
                Type::Array { elem: ae, .. } | Type::OpenArray { elem: ae } => {
                    self.same_type(*elem, *ae)
                }
                Type::StringLit(_) => self.is_char(*elem),
                _ => false,
            };
        }
        if formal.by_var {
            if self.same_type(formal.ty, actual) {
                return true;
            }
            // "except in the case of a record, where it must be a base type of
            //  the corresponding actual parameter's type"
            return match (self.get(formal.ty), self.get(actual)) {
                (Type::Record { .. }, Type::Record { .. }) => self.extends(actual, formal.ty),
                _ => false,
            };
        }
        self.is_assignable(formal.ty, actual)
    }

    // operators

    /// The operator rules for `SimpleExpression` and `term` are in report sections
    /// 8.2.1-8.2.3. `Err` carries the reason, which becomes the diagnostic's note.
    pub fn operator_ok(&self, op: BinOp, l: TypeId, r: TypeId) -> Result<(), &'static str> {
        use BinOp::*;
        if self.is_error(l) || self.is_error(r) {
            return Ok(());
        }
        let same = self.same_type(l, r) || (self.is_integer_like(l) && self.is_integer_like(r));
        match op {
            Or | And => {
                if self.is_bool(l) && self.is_bool(r) {
                    Ok(())
                } else {
                    Err("this operator applies to BOOLEAN operands (report 8.2.1)")
                }
            }
            Add | Sub | Mul | Div => {
                // Numeric operands must have the same type (section 8.2.2); SET
                // operands use union, difference, intersection or symmetric difference
                // (section 8.2.3).
                let numeric_same_type = self.is_numeric(l) && same;
                let both_sets = self.is_set(l) && self.is_set(r);
                if numeric_same_type || both_sets {
                    Ok(())
                } else if self.is_numeric(l) || self.is_numeric(r) {
                    Err("both operands of this operator must have the same type (report 8.2.2)")
                } else {
                    Err("this operator applies to numeric or SET operands (report 8.2.2, 8.2.3)")
                }
            }
            IDiv | Mod => {
                if self.is_integer_like(l) && self.is_integer_like(r) {
                    Ok(())
                } else {
                    Err("DIV and MOD apply to integer operands only (report 8.2.2)")
                }
            }
            In => {
                if self.is_integer_like(l) && self.is_set(r) {
                    Ok(())
                } else {
                    Err("`x IN s` needs an INTEGER x and a SET s (report 8.2.4)")
                }
            }
            Eq | Ne => {
                // Section 8.2.4 applies `=` and `#` to BOOLEAN, SET, numeric types,
                // CHAR and character arrays, and pointer and procedure types.
                // Both operands must have the same type. A relation is not a conversion.
                let comparable = |this: &Self, a: TypeId, b: TypeId| -> bool {
                    if this.is_error(a) || this.is_error(b) {
                        return true;
                    }
                    let same = this.same_type(a, b)
                        || (this.is_integer_like(a) && this.is_integer_like(b));
                    if this.is_nil(a) || this.is_nil(b) {
                        // NIL is comparable with a pointer or a procedure value.
                        let other = if this.is_nil(a) { b } else { a };
                        return this.is_pointer(other) || this.is_proc(other);
                    }
                    if this.is_numeric(a) && this.is_numeric(b) {
                        return same;
                    }
                    if this.is_char_like(a) && this.is_char_like(b) {
                        return true;
                    }
                    if this.is_char_array(a) && this.is_char_array(b) {
                        return true;
                    }
                    if this.is_bool(a) && this.is_bool(b) {
                        return true;
                    }
                    if this.is_set(a) && this.is_set(b) {
                        return true;
                    }
                    if (this.is_pointer(a) && this.is_pointer(b))
                        || (this.is_proc(a) && this.is_proc(b))
                    {
                        return same;
                    }
                    false
                };
                if comparable(self, l, r) {
                    Ok(())
                } else {
                    Err("= and # do not apply to these operand types (report 8.2.4)")
                }
            }
            Lt | Le | Gt | Ge => {
                if self.is_error(l) || self.is_error(r) {
                    return Ok(());
                }
                let ordered = (self.is_numeric(l) && self.is_numeric(r) && same)
                    || (self.is_char_like(l) && self.is_char_like(r))
                    || (self.is_char_array(l) && self.is_char_array(r));
                if ordered {
                    Ok(())
                } else {
                    Err(
                        "the ordering relations apply to the numeric types, CHAR and \
                         character arrays (report 8.2.4)",
                    )
                }
            }
        }
    }

    /// Unary operators (report sections 8.2.1, 8.2.2 and 8.2.3). A SET operand makes
    /// `-` the complement. The `+` operator is the identity on numeric operands.
    pub fn unary_ok(&self, op: syntax::ast::UnOp, t: TypeId) -> Result<(), &'static str> {
        use syntax::ast::UnOp::*;
        if self.is_error(t) {
            return Ok(());
        }
        match op {
            Not => {
                if self.is_bool(t) {
                    Ok(())
                } else {
                    Err("~ applies to BOOLEAN operands (report 8.2.1)")
                }
            }
            Neg => {
                if self.is_numeric(t) || self.is_set(t) {
                    Ok(())
                } else {
                    Err("- applies to numeric or SET operands (report 8.2.2, 8.2.3)")
                }
            }
            Plus => {
                if self.is_numeric(t) {
                    Ok(())
                } else {
                    Err("+ applies to numeric operands (report 8.2.2)")
                }
            }
        }
    }

    // predeclared procedures

    /// This method checks the signatures in report section 10.2. It returns the result
    /// type or the reason the arguments do not fit. `LEN` of a fixed array folds to a
    pub fn predefined(&mut self, name: &str, args: &[TypeId]) -> Result<TypeId, String> {
        let arity = |n: usize| -> Result<(), String> {
            if args.len() == n {
                Ok(())
            } else {
                Err(format!("{name} takes {n} argument(s), got {}", args.len()))
            }
        };
        let one = |s: &mut Self, f: &dyn Fn(&Self, TypeId) -> bool, what: &str| -> Result<TypeId, String> {
            arity(1)?;
            if args.iter().any(|t| s.is_error(*t)) {
                return Ok(s.error());
            }
            if f(s, args[0]) {
                Ok(args[0])
            } else {
                Err(format!("{name} does not accept {what}"))
            }
        };
        match name {
            "ABS" => one(
                self,
                &|s, t| s.is_numeric(t),
                "a non-numeric operand (report 10.2)",
            ),
            "ODD" => {
                arity(1)?;
                if !self.is_integer_like(args[0]) {
                    return Err("ODD takes an INTEGER (report 10.2)".into());
                }
                Ok(self.boolean())
            }
            "LEN" => {
                arity(1)?;
                if self.elem(args[0]).is_none() {
                    return Err("LEN takes an array (report 10.2)".into());
                }
                Ok(self.integer())
            }
            "LSL" | "ASR" | "ROR" => {
                arity(2)?;
                if !self.is_integer_like(args[0]) || !self.is_integer_like(args[1]) {
                    return Err(format!("{name} takes two INTEGERs (report 10.2)"));
                }
                Ok(self.integer())
            }
            "FLOOR" => {
                arity(1)?;
                if !matches!(self.get(args[0]), Type::Basic(Basic::Real)) {
                    return Err("FLOOR takes a REAL (report 10.2)".into());
                }
                Ok(self.integer())
            }
            "FLT" => {
                arity(1)?;
                if !self.is_integer_like(args[0]) {
                    return Err("FLT takes an INTEGER (report 10.2)".into());
                }
                Ok(self.real())
            }
            "ORD" => {
                arity(1)?;
                if self.is_char_like(args[0]) || self.is_bool(args[0]) || self.is_set(args[0]) {
                    Ok(self.integer())
                } else {
                    Err("ORD takes a CHAR, BOOLEAN or SET (report 10.2)".into())
                }
            }
            "CHR" => {
                arity(1)?;
                if self.is_integer_like(args[0]) {
                    Ok(self.char())
                } else {
                    Err("CHR takes an INTEGER (report 10.2)".into())
                }
            }
            _ => Err(format!("`{name}` is not a predeclared function procedure")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use syntax::ast::UnOp;

    /// A record field for the type-algebra tests: the mark only matters across an
    /// interface, so these are public.
    fn field(name: &str, ty: TypeId) -> Field {
        Field {
            name: name.to_string(),
            ty,
            export: true,
        }
    }

    /// A small universe modeled on the report's own examples (sections 6.3 and 6.4).
    struct World {
        t: Types,
        node: TypeId,
        center: TypeId,
        p_node: TypeId,
        p_center: TypeId,
        char4: TypeId,
    }

    fn world() -> World {
        let mut t = Types::new();
        let int = t.integer();
        let node = t.record(None, vec![field("key", int)]);
        let name_ty = t.array(32, t.char());
        let center = t.record(
            Some(node),
            vec![field("name", name_ty), field("sub", node)],
        );
        let p_node = t.pointer(node);
        let p_center = t.pointer(center);
        let char4 = t.array(4, t.char());
        World {
            t,
            node,
            center,
            p_node,
            p_center,
            char4,
        }
    }

    #[test]
    fn basic_identity_and_compatibility() {
        let t = Types::new();
        let (i, b, r) = (t.integer(), t.byte(), t.real());
        assert!(t.same_type(i, i));
        assert!(!t.same_type(i, b), "BYTE is compatible with, not equal to, INTEGER");
        assert!(!t.same_type(i, r));
        // Report section 6.1: both directions.
        assert!(t.is_assignable(i, b));
        assert!(t.is_assignable(b, i));
        // Report section 8.2.2: no implicit REAL/INTEGER conversion.
        assert!(!t.is_assignable(r, i));
        assert!(!t.is_assignable(i, r));
    }

    #[test]
    fn arrays_are_structural_records_are_not() {
        let mut t = Types::new();
        let a = t.array(4, t.integer());
        let b = t.array(4, t.integer());
        let c = t.array(5, t.integer());
        let d = t.array(4, t.byte());
        assert!(t.same_type(a, b));
        assert!(!t.same_type(a, c));
        assert!(!t.same_type(a, d));

        // Records with identical fields remain distinct types (report section 6.3).
        let f = vec![field("x", t.integer())];
        let r1 = t.record(None, f.clone());
        let r2 = t.record(None, f);
        assert!(!t.same_type(r1, r2));
        assert!(t.same_type(r1, r1));
    }

    #[test]
    fn pointer_identity_and_extension_follow_the_base() {
        let mut w = world();
        // Report section 6.4: `POINTER TO T` is the same type however it is written.
        let again = w.t.pointer(w.node);
        assert!(w.t.same_type(again, w.p_node));
        // and P extends P0 exactly when its base extends P0's base
        assert!(w.t.extends(w.p_center, w.p_node));
        assert!(!w.t.extends(w.p_node, w.p_center));
        assert!(w.t.extends(w.p_node, w.p_node));
    }

    #[test]
    fn record_extension_is_reflexive_transitive_and_one_way() {
        let mut w = world();
        let int = w.t.integer();
        let deep = w.t.record(Some(w.center), vec![field("z", int)]);
        assert!(w.t.extends(w.node, w.node));
        assert!(w.t.extends(w.center, w.node));
        assert!(w.t.extends(deep, w.center));
        assert!(w.t.extends(deep, w.node), "extends is transitive (report 6.3)");
        assert!(!w.t.extends(w.node, w.center));
        assert!(!w.t.extends(int, w.node));
    }

    #[test]
    fn field_lookup_walks_the_base_chain() {
        let w = world();
        assert_eq!(w.t.field(w.center, "key"), Some(w.t.integer()));
        assert_eq!(w.t.field(w.center, "name").map(|t| w.t.array_len(t)), Some(Some(32)));
        assert_eq!(w.t.field(w.node, "name"), None);
        assert_eq!(w.t.field(w.t.integer(), "key"), None);
    }

    #[test]
    fn assignment_exceptions_of_report_9_1() {
        let mut w = world();
        // (`w` is `mut` because interning new types borrows the interner.)
        let char = w.t.char();
        let three = w.t.string_lit(3);
        let four = w.t.string_lit(4);

        // 9.1.1: NIL to any pointer or procedure type
        assert!(w.t.is_assignable(w.p_node, w.t.nil()));
        let pp = w.t.proc(vec![], None);
        assert!(w.t.is_assignable(pp, w.t.nil()));
        assert!(!w.t.is_assignable(w.t.integer(), w.t.nil()));

        // 9.1.2: shorter than the array, NUL appended; a one-character string to CHAR
        assert!(w.t.is_assignable(w.char4, three));
        assert!(!w.t.is_assignable(w.char4, four), "len(src) < n is required");
        let one = w.t.string_lit(1);
        let two = w.t.string_lit(2);
        assert!(w.t.is_assignable(char, one));
        assert!(!w.t.is_assignable(char, two));

        // 9.1.3: records, source extends destination
        assert!(w.t.is_assignable(w.node, w.center));
        assert!(!w.t.is_assignable(w.center, w.node));
        // Section 6.4 also gives pointers this rule: `p := q` with q: POINTER TO CenterNode
        assert!(w.t.is_assignable(w.p_node, w.p_center));
        assert!(!w.t.is_assignable(w.p_center, w.p_node), "a base pointer needs a guard");
        // pointers do not mix with records
        assert!(!w.t.is_assignable(w.p_center, w.center));
        assert!(!w.t.is_assignable(w.center, w.p_center));

        // 9.1.4: an open array to an array of equal base type
        let open_int = w.t.open_array(w.t.integer());
        let int5 = w.t.array(5, w.t.integer());
        assert!(w.t.is_assignable(int5, open_int));
        assert!(!w.t.is_assignable(w.char4, open_int));

        // a record may not go into a pointer, and vice versa
        assert!(!w.t.is_assignable(w.p_node, w.node));
        assert!(!w.t.is_assignable(w.node, w.p_node));
    }

    #[test]
    fn parameter_modes_of_report_10_1() {
        let mut w = world();
        let int = w.t.integer();
        let char4 = w.char4;
        let open_char = w.t.open_array(w.t.char());

        // value parameter: an open array of CHAR accepts any length and a literal
        let f = Param { by_var: false, ty: open_char };
        let char64 = w.t.array(64, w.t.char());
        let lit3 = w.t.string_lit(3);
        let int4 = w.t.array(4, int);
        assert!(w.t.param_ok(&f, char4));
        assert!(w.t.param_ok(&f, char64));
        assert!(w.t.param_ok(&f, lit3));
        assert!(!w.t.param_ok(&f, int4));

        // VAR parameter: identical types ...
        let v = Param { by_var: true, ty: int };
        assert!(w.t.param_ok(&v, int));
        assert!(!w.t.param_ok(&v, w.t.byte()), "VAR needs identical, not merely compatible");
        assert!(!w.t.param_ok(&v, w.t.real()));

        // ... except that a record formal may be a base type of the actual
        let rv = Param { by_var: true, ty: w.node };
        assert!(w.t.param_ok(&rv, w.center));
        assert!(!w.t.param_ok(&rv, w.p_center));
        // a VAR open array takes any length but the same element type
        let vo = Param { by_var: true, ty: w.t.open_array(int) };
        let int7 = w.t.array(7, int);
        assert!(w.t.param_ok(&vo, int7));
        assert!(!w.t.param_ok(&vo, char4));
    }

    #[test]
    fn relation_rules_of_report_8_2_4() {
        let mut w = world();
        let (int, real, bool_, set, char) = (
            w.t.integer(),
            w.t.real(),
            w.t.boolean(),
            w.t.set(),
            w.t.char(),
        );
        use BinOp::*;
        // ordering: numerics, CHAR, CHAR arrays
        assert!(w.t.operator_ok(Lt, int, int).is_ok());
        assert!(w.t.operator_ok(Lt, real, real).is_ok());
        assert!(w.t.operator_ok(Lt, char, char).is_ok());
        assert!(w.t.operator_ok(Lt, w.char4, w.char4).is_ok());
        assert!(w.t.operator_ok(Lt, int, real).is_err(), "mixed numeric types");
        assert!(w.t.operator_ok(Lt, bool_, bool_).is_err());
        // both operands matter: `1 = TRUE` is not a relation (report 8.2.4)
        assert!(w.t.operator_ok(Eq, int, bool_).is_err());
        assert!(w.t.operator_ok(Eq, int, real).is_err());
        assert!(w.t.operator_ok(Ne, char, int).is_err());
        assert!(w.t.operator_ok(Eq, int, int).is_ok());
        assert!(w.t.operator_ok(Lt, set, set).is_err());
        // `=` and `#` also apply to BOOLEAN, SET, pointers, procedures and NIL.
        assert!(w.t.operator_ok(Eq, bool_, bool_).is_ok());
        assert!(w.t.operator_ok(Eq, set, set).is_ok());
        assert!(w.t.operator_ok(Eq, w.p_node, w.p_node).is_ok());
        assert!(w.t.operator_ok(Eq, w.p_node, w.t.nil()).is_ok());
        let np = w.t.proc(vec![], None);
        let nil = w.t.nil();
        assert!(w.t.operator_ok(Eq, np, nil).is_ok());
        // IN
        assert!(w.t.operator_ok(In, int, set).is_ok());
        assert!(w.t.operator_ok(In, real, set).is_err());
        assert!(w.t.operator_ok(In, int, int).is_err());
    }

    #[test]
    fn type_tests_and_guards_need_the_extension_relation() {
        let w = world();
        // Report sections 8.1 and 8.2.4: T0 must extend v's declared type.
        assert!(w.t.test_applicable(w.p_node, w.p_center));
        assert!(w.t.test_applicable(w.p_node, w.p_node));
        assert!(!w.t.test_applicable(w.p_center, w.p_node));
        assert!(w.t.test_applicable(w.node, w.center));
        // not a pointer or record: the guard cannot occur at all
        assert!(!w.t.test_applicable(w.t.integer(), w.node));
    }

    #[test]
    fn arithmetic_and_set_operators() {
        let w = world();
        let (int, real, bool_, set) = (
            w.t.integer(),
            w.t.real(),
            w.t.boolean(),
            w.t.set(),
        );
        use BinOp::*;
        assert!(w.t.operator_ok(Add, int, int).is_ok());
        assert!(w.t.operator_ok(Add, real, real).is_ok());
        assert!(w.t.operator_ok(Add, int, real).is_err());
        assert!(w.t.operator_ok(Div, int, int).is_ok(), "`/` is a numeric operator");
        assert!(w.t.operator_ok(IDiv, int, int).is_ok());
        assert!(w.t.operator_ok(IDiv, real, real).is_err());
        assert!(w.t.operator_ok(Mod, int, int).is_ok());
        assert!(w.t.operator_ok(Or, bool_, bool_).is_ok());
        assert!(w.t.operator_ok(Or, int, int).is_err());
        assert!(w.t.operator_ok(And, bool_, bool_).is_ok());
        // Report section 8.2.3: set operators use the same symbols as arithmetic.
        assert!(w.t.operator_ok(Add, set, set).is_ok());
        assert!(w.t.operator_ok(Sub, set, set).is_ok());
        assert!(w.t.operator_ok(Mul, set, set).is_ok());
        assert!(w.t.operator_ok(Div, set, set).is_ok());
        assert!(w.t.operator_ok(Add, set, int).is_err());

        assert!(w.t.unary_ok(UnOp::Not, bool_).is_ok());
        assert!(w.t.unary_ok(UnOp::Not, int).is_err());
        assert!(w.t.unary_ok(UnOp::Neg, int).is_ok());
        assert!(w.t.unary_ok(UnOp::Neg, set).is_ok(), "single SET operand is the complement");
        assert!(w.t.unary_ok(UnOp::Neg, bool_).is_err());
        assert!(w.t.unary_ok(UnOp::Plus, real).is_ok());
        assert!(w.t.unary_ok(UnOp::Plus, set).is_err());
    }

    #[test]
    fn predeclared_signatures_of_report_10_2() {
        let mut w = world();
        let int = w.t.integer();
        let real = w.t.real();
        let char = w.t.char();
        let bool_ = w.t.boolean();
        let set = w.t.set();
        let char4 = w.char4;
        let numeric = w.t.array(2, int);

        assert_eq!(w.t.predefined("ABS", &[int]).unwrap(), int);
        assert_eq!(w.t.predefined("ABS", &[real]).unwrap(), real);
        assert!(w.t.predefined("ABS", &[set]).is_err());
        assert_eq!(w.t.predefined("ODD", &[int]).unwrap(), bool_);
        assert!(w.t.predefined("ODD", &[real]).is_err());
        assert_eq!(w.t.predefined("LEN", &[char4]).unwrap(), int);
        let open_char = w.t.open_array(char);
        assert_eq!(w.t.predefined("LEN", &[open_char]).unwrap(), int);
        assert!(w.t.predefined("LEN", &[int]).is_err());
        assert_eq!(w.t.predefined("LSL", &[int, int]).unwrap(), int);
        assert_eq!(w.t.predefined("ASR", &[int, int]).unwrap(), int);
        assert_eq!(w.t.predefined("ROR", &[int, int]).unwrap(), int);
        assert!(w.t.predefined("LSL", &[int]).is_err());
        assert_eq!(w.t.predefined("FLOOR", &[real]).unwrap(), int);
        assert!(w.t.predefined("FLOOR", &[int]).is_err());
        assert_eq!(w.t.predefined("FLT", &[int]).unwrap(), real);
        assert_eq!(w.t.predefined("ORD", &[char]).unwrap(), int);
        assert_eq!(w.t.predefined("ORD", &[bool_]).unwrap(), int);
        assert_eq!(w.t.predefined("ORD", &[set]).unwrap(), int);
        assert!(w.t.predefined("ORD", &[real]).is_err());
        assert_eq!(w.t.predefined("CHR", &[int]).unwrap(), char);
        assert!(w.t.predefined("CHR", &[char]).is_err());
        assert!(w.t.predefined("NOPE", &[int]).is_err());
        // error types do not cascade (report: one diagnostic per mistake)
        assert_eq!(w.t.predefined("ABS", &[w.t.error()]).unwrap(), w.t.error());
        let _ = numeric;
    }

    #[test]
    fn same_type_is_not_used_to_compare_procedure_types_loosely() {
        let mut t = Types::new();
        let int = t.integer();
        let f1 = t.proc(vec![Param { by_var: false, ty: int }], Some(int));
        let f2 = t.proc(vec![Param { by_var: false, ty: int }], Some(int));
        let f3 = t.proc(vec![Param { by_var: true, ty: int }], Some(int));
        let f4 = t.proc(vec![Param { by_var: false, ty: int }], None);
        assert!(t.same_type(f1, f2), "procedure types are structural (report 6.5)");
        assert!(!t.same_type(f1, f3), "the parameter mode is part of the type");
        assert!(!t.same_type(f1, f4), "the result type is part of the type");
    }
}
