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

use syntax::ast::Module;
use cgen::cgen::{mangle, span_of_type};
use sema::sema::Sema;
use sema::types::{Basic, Type, TypeId, Types};

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

/// What a field access needs of a record: the base it embeds (whose fields are
/// its own, at the base's offsets), and its *own* fields with the offsets and
/// types the C struct has. Both the descriptor and the base are at offset 0
/// (§9.3, and the facts `e2e`'s comparison pins: `Cell.desc offset=0`,
/// `Paired.base offset=0`), so nothing here has to carry them.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RecordLayout {
    pub base: Option<TypeId>,
    pub fields: Vec<(String, u64, TypeId)>,
}

/// Everything a back end keeps after the walk: a size/alignment per type, and a
/// record's own fields for whoever has to address one.
#[derive(Default)]
pub struct Tables {
    pub layouts: HashMap<TypeId, Layout>,
    pub records: HashMap<TypeId, RecordLayout>,
}

/// The layouts of one module's types, memoized: a record's layout is its
/// members' layouts, and the type graph is a DAG through arrays and bases (a
/// record reached through a pointer is not a member, so the walk terminates).
pub struct Layouts<'a> {
    types: &'a Types,
    pointer: u64,
    cache: HashMap<TypeId, Option<Layout>>,
    members: HashMap<TypeId, Vec<Member>>,
    records: HashMap<TypeId, RecordLayout>,
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
            records: HashMap::new(),
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
            // A pointer, and a procedure value, are an address — and the walk
            // continues into what a pointer points at, because `NEW`, a field
            // access and the collector's map all need *that* type's layout and
            // members. The recursion terminates: a pointer's base is a declared
            // type, so a cycle goes through a pointer.
            Type::Pointer { base } => {
                // The pointer's own layout is known before the walk goes on, so a
                // cycle through it (`P = POINTER TO Node; Node = RECORD next: P
                // END`) terminates: the cache is filled here rather than after the
                // recursion.
                let layout = Some(Layout::new(self.pointer, self.pointer));
                self.cache.insert(t, layout);
                let _ = self.of(base);
                return layout;
            }
            Type::Proc { .. } => Some(Layout::new(self.pointer, self.pointer)),
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
                let mut own = Vec::new();
                for f in fields {
                    let fl = self.of(f.ty)?;
                    offset = round_up(offset, fl.align);
                    members.push(Member {
                        name: mangle(&f.name),
                        offset,
                    });
                    own.push((mangle(&f.name), offset, f.ty));
                    offset += fl.size;
                    align = align.max(fl.align);
                }
                self.members.insert(t, members);
                self.records.insert(
                    t,
                    RecordLayout {
                        base,
                        fields: own,
                    },
                );
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
    pub fn resolved(self) -> Tables {
        Tables {
            layouts: self
                .cache
                .into_iter()
                .filter_map(|(t, l)| l.map(|l| (t, l)))
                .collect(),
            records: self.records,
        }
    }

    /// A record's own fields, with the offsets a `Field` selector adds to. It
    /// walks the base chain, so an inherited field is found where the derived
    /// record's layout puts it.
    pub fn field_offset(&mut self, rec: TypeId, name: &str) -> Option<(u64, TypeId)> {
        let mut cur = rec;
        for _ in 0..64 {
            let r = self.record_of(cur)?;
            if let Some((_, off, ty)) = r.fields.iter().find(|(n, _, _)| n == name) {
                return Some((*off, *ty));
            }
            cur = r.base?;
        }
        None
    }

    /// A record's own fields, in declaration order.
    pub fn record_of(&mut self, rec: TypeId) -> Option<RecordLayout> {
        if !self.records.contains_key(&rec) {
            let _ = self.of(rec);
        }
        self.records.get(&rec).cloned()
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

/// The offsets inside the runtime's `ObDesc`, which the C side has as a struct
/// (`oberon.h`): `{ const ObDesc *base; int32_t id; const char *name; const
/// ObFld *fields; int32_t nfields; }`. It is a fixed shape rather than a
/// program's type, so it is computed here once — with the same padding rule as
/// everything else — instead of being written out as literals that a change to
/// the C struct would silently falsify. The collector reads `fields`/`nfields`
/// on every mark, so a mistake here is a crash rather than a wrong number.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ObDesc {
    pub size: u64,
    pub base: u64,
    pub id: u64,
    pub name: u64,
    pub fields: u64,
    pub nfields: u64,
}

/// The same for `ObFld { size_t off; size_t stride; size_t n; const ObDesc
/// *desc; }` — all `size_t`, so every member is pointer-aligned.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ObFld {
    pub size: u64,
    pub off: u64,
    pub stride: u64,
    pub n: u64,
    pub desc: u64,
}

pub fn ob_desc(pointer_bytes: u64) -> ObDesc {
    let p = pointer_bytes;
    let mut off = 0;
    let base = off;
    off += p;
    let id = off;
    off += 4;
    off = round_up(off, p);
    let name = off;
    off += p;
    let fields = off;
    off += p;
    let nfields = off;
    off += 4;
    ObDesc {
        size: round_up(off, p),
        base,
        id,
        name,
        fields,
        nfields,
    }
}

pub fn ob_fld(pointer_bytes: u64) -> ObFld {
    let p = pointer_bytes;
    ObFld {
        size: 4 * p,
        off: 0,
        stride: p,
        n: 2 * p,
        desc: 3 * p,
    }
}

/// The next multiple of `align` at or after `off`. `align` is a power of two, so
/// this is the mask the C ABI's padding is.
pub(crate) fn round_up(off: u64, align: u64) -> u64 {
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
