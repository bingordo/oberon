//! The layout of the types both back ends emit, computed rather than delegated.
//!
//! The C back end does not compute a layout at all: it emits C structs
//! (`struct R { const ObDesc *desc; int32_t x; }`, an extension's base as its
//! first member) and lets the C compiler place the members — `field_path`
//! produces a *member path*, and `rt_new` is handed `sizeof(T)` (DESIGN §16).
//! So there is nothing to share, and this module's whole job is to reproduce
//! what `cc` does for those structs, for the shapes this compiler emits: scalars,
//! fixed arrays and records (with an extension embedding its base first).
//!
//! Being a reproduction is why it is *checked* rather than trusted: `e2e`'s
//! layout block compares this table, fact by fact, with `sizeof`/`offsetof`
//! computed by `cc` from the very header the C back end wrote, so `cc` is the
//! authority and this file is the claim. A second back end that allocated a
//! record the C side reads (or the other way round) depends on the two agreeing
//! byte for byte.
//!
//! The four rules that are decisions rather than arithmetic, each of them the C
//! ABI's and each one pinned by that comparison:
//!
//! * `char` (and `BYTE`) is one byte with alignment 1, so a `CHAR` between two
//!   `INTEGER`s is packed into the padding rather than shifting the next word;
//! * a pointer has the *target's* width and alignment — taken from the ISA the
//!   object is built for, never from a literal — which is what pads a field
//!   after an odd number of words;
//! * a record is its members in declaration order, each at the next multiple of
//!   its own alignment, and the whole padded to the largest of them: `struct {
//!   int32_t x; char c; }` is 8 bytes with `c` at 4;
//! * every record has a descriptor word at offset 0 — in the base-most record of
//!   a chain (extensions embed their base first, so the word is at offset 0 of
//!   every view, which is what `IS`, a guard and a type-`CASE` read: §9.3) — and
//!   that is also why `RECORD END` is one pointer and not nothing.

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::ast::Module;
use crate::cgen::{mangle, span_of_type};
use crate::sema::Sema;
use crate::types::{Basic, Type, TypeId, Types};

/// A type's size and alignment, in bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Layout {
    pub size: u64,
    pub align: u64,
}

impl Layout {
    fn new(size: u64, align: u64) -> Layout {
        Layout { size, align }
    }
}

/// One member of a record, as the C emitter spells it: `desc` in a base-most
/// record, `base` in an extension, then the declared fields, escaped through the
/// same `mangle` the C back end uses so that a field named `int` is `o_int` on
/// both sides.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Member {
    pub name: String,
    pub offset: u64,
}

/// The layouts of one module's types, memoized: a record's layout is its
/// members' layouts, and the type graph is a DAG through arrays and bases (a
/// record reached through a pointer is not a member, so the walk terminates).
pub struct Layouts<'a> {
    types: &'a Types,
    pointer: u64,
    cache: HashMap<TypeId, Option<Layout>>,
    members: HashMap<TypeId, Vec<Member>>,
}

impl<'a> Layouts<'a> {
    /// `pointer_bytes` is the target's pointer width — 8 for the hosts this
    /// compiler has been built on, but read from the ISA rather than assumed.
    pub fn new(types: &'a Types, pointer_bytes: u64) -> Layouts<'a> {
        Layouts {
            types,
            pointer: pointer_bytes,
            cache: HashMap::new(),
            members: HashMap::new(),
        }
    }

    pub fn pointer_bytes(&self) -> u64 {
        self.pointer
    }

    /// The layout of a type, or `None` for one that has no storage: an error
    /// type from a rejected program, `NIL`, a string literal, and an open array
    /// (which is only ever a formal parameter). None of those can be a record
    /// member or an array element in a module that checks.
    pub fn of(&mut self, t: TypeId) -> Option<Layout> {
        if let Some(cached) = self.cache.get(&t) {
            return *cached;
        }
        let layout = match self.types.get(t).clone() {
            // One byte, alignment 1: the C emitter's `char`/`uint8_t` (§9.2).
            Type::Basic(Basic::Char) | Type::Basic(Basic::Byte) => Some(Layout::new(1, 1)),
            // `int32_t`, `float` and `uint32_t` are all one word (and so is
            // `BOOLEAN`, which the C back end emits as an `int32_t`).
            Type::Basic(_) => Some(Layout::new(4, 4)),
            // A pointer, and a procedure value, are an address.
            Type::Pointer { .. } | Type::Proc { .. } => {
                Some(Layout::new(self.pointer, self.pointer))
            }
            // `struct { T e[n]; }`: no trailing padding inside the array, and the
            // element's alignment is the array's.
            Type::Array { len, elem } => {
                let e = self.of(elem)?;
                Some(Layout::new(u64::from(len) * e.size, e.align))
            }
            Type::Record { base, fields, .. } => {
                let (mut offset, mut align) = match base {
                    // An extension's base is its first member, so the base's
                    // layout is the prefix of the derived one's.
                    Some(b) => {
                        let bl = self.of(b)?;
                        (bl.size, bl.align)
                    }
                    // The descriptor word, at offset 0 of the base-most record.
                    None => (self.pointer, self.pointer),
                };
                let mut members = Vec::new();
                if base.is_none() {
                    members.push(Member {
                        name: "desc".to_string(),
                        offset: 0,
                    });
                } else {
                    members.push(Member {
                        name: "base".to_string(),
                        offset: 0,
                    });
                }
                for f in fields {
                    let fl = self.of(f.ty)?;
                    offset = round_up(offset, fl.align);
                    members.push(Member {
                        name: mangle(&f.name),
                        offset,
                    });
                    offset += fl.size;
                    align = align.max(fl.align);
                }
                self.members.insert(t, members);
                Some(Layout::new(round_up(offset, align), align))
            }
            Type::OpenArray { .. } | Type::Nil | Type::StringLit(_) | Type::Error => None,
        };
        self.cache.insert(t, layout);
        layout
    }

    /// The layouts every type asked for since `new`, without the ones that have
    /// no storage: the module-wide table a back end keeps instead of borrowing
    /// this walk at every use.
    pub fn resolved(self) -> HashMap<TypeId, Layout> {
        self.cache
            .into_iter()
            .filter_map(|(t, l)| l.map(|l| (t, l)))
            .collect()
    }

    /// The members a record *declares*: its descriptor or base, then its own
    /// fields, in the order the C struct has them.
    pub fn members(&mut self, rec: TypeId) -> Vec<Member> {
        if !self.members.contains_key(&rec) {
            let _ = self.of(rec);
        }
        self.members.get(&rec).cloned().unwrap_or_default()
    }
}

/// The next multiple of `align` at or after `off`. `align` is a power of two, so
/// this is the mask the C ABI's padding is.
fn round_up(off: u64, align: u64) -> u64 {
    (off + align - 1) & !(align - 1)
}

/// The table `obc emit --layout` prints: one fact per line, in the order the
/// comparison in `e2e` reads them — a type's size and alignment, then each
/// member the C struct has at its offset. One line per fact rather than a nested
/// format, because the other side of the comparison is a C program printing
/// `sizeof` and `offsetof`, and the check is line for line.
///
/// The facts are the module's `TYPE` section, which is what a fixture can carry
/// and what a client of the module needs; a type declared inside a procedure has
/// no client and is not listed.
pub fn table(module: &Module, types: &Types, sema: &Sema<'_>, pointer_bytes: u64) -> String {
    let mut layouts = Layouts::new(types, pointer_bytes);
    let mut out = String::new();
    for decl in &module.decls.types {
        let id = sema.type_at(span_of_type(&decl.ty), types).unwrap_or(TypeId(0));
        let Some(layout) = layouts.of(id) else {
            continue;
        };
        let _ = writeln!(
            out,
            "{} size={} align={}",
            decl.name, layout.size, layout.align
        );
        for m in layouts.members(id) {
            let _ = writeln!(out, "{}.{} offset={}", decl.name, m.name, m.offset);
        }
    }
    out
}
