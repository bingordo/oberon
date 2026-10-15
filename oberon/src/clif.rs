//! The Cranelift back end (DESIGN §16.2): the same checked module, lowered to
//! Cranelift IR and emitted as an object file.
//!
//! It sits where `cgen` does — the phase boundary the design fixes — and
//! consumes the same checked AST and `sema` types, so a program is a program
//! whichever back end takes it. What differs is what the *backend* decides:
//! integer arithmetic wraps because the IR defines it that way (which is what
//! `-fwrapv` buys the C path), the shifts follow the report rather than the IR's
//! masking, traversal order is the source's because there is no C compiler to
//! reorder it, and every check is a call into the runtime, which stays C
//! (DESIGN §16.2 records the interface that fixes).
//!
//! The fragment this milestone lowers is the fuzz corpus's: `INTEGER` and
//! `BOOLEAN` variables, one-dimensional fixed arrays of them, `+ - * DIV MOD /`,
//! `ABS`, `ODD`, `MIN`/`MAX`, the three shifts, relations, assignment,
//! `IF`/`WHILE`/`REPEAT`/`FOR`/`CASE`/`EXIT`/`RETURN`, `ASSERT`, procedures
//! with value parameters, and recursion. Anything else is refused by name, with
//! the milestone that owes it (PLAN.md W9.2), rather than lowered to something
//! plausible: a second back end that silently disagrees with the first is worse
//! than one that says what it does not do.
//!
//! Locals become SSA values (`FunctionBuilder`'s `Variable`, which is what puts
//! a block argument in where a loop needs one), while module variables are data
//! objects reached by address, exactly as the C back end's statics are. The
//! module's `__init`, `__load` and command table are the ones the driver's
//! loader already calls, so a build links either back end's output.

use std::collections::HashMap;

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{
    self, types, AbiParam, InstBuilder, Signature, StackSlotData, StackSlotKind, TrapCode,
    UserFuncName,
};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module as ClifModule};
use cranelift_object::{ObjectBuilder, ObjectModule};
use target_lexicon::BinaryFormat;

use crate::ast::{
    BinOp, CaseArm, CaseLabel, Designator, Expr, ExprKind, Module, Proc, Selector, Span, Stmt,
    StmtKind, UnOp,
};
use crate::cgen::{self, mangle, span_of_type};
use crate::const_eval;
use crate::layout::{Layout, Layouts};
use crate::sema::{span_key, Sema};
use crate::types::{Basic, Type, TypeId, Types};
use crate::Diagnostic;

/// A formal parameter as the checker flattened it: its name, and its type.
type Formal = (String, TypeId);

/// `INTEGER` and `BOOLEAN` are one IR type here (DESIGN §9.2: an `int32_t`, with
/// 0/1 for the truth values, so `ORD(p)` is the C value).
const I32: ir::Type = types::I32;

/// The width of an `INTEGER`, and so the stride of an array of them.
const WORD: u64 = 4;

/// A message a trap this back end reports itself carries, byte for byte the C
/// back end's: the arithmetic checks' messages belong to the runtime helper that
/// reports them, and these two are the emitter's own decisions (DESIGN §9.4's
/// table, and the corpus's adversarial mode compares the text and the byte).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Msg {
    /// `ASSERT` (report §10.2, DESIGN §9.6).
    Assertion,
    /// A `CASE` whose arms cannot match the subject: the report leaves it
    /// undefined, and W2.5's decision — which the C back end implements — is a
    /// located trap rather than a silent fall-through.
    CaseDefault,
    /// A *type*-`CASE` with no matching label (DESIGN §9.3's trapping default,
    /// which the C back end has always had).
    CaseType,
    /// A `NIL` pointer where an object was expected: the C back end's `rt_nil`
    /// message, with the designator's byte.
    Nil,
}

impl Msg {
    fn text(self) -> &'static str {
        match self {
            Msg::Assertion => "assertion failed",
            Msg::CaseDefault => "CASE: no matching label",
            Msg::CaseType => "CASE: no matching type",
            Msg::Nil => "NIL pointer dereference",
        }
    }

    /// The data object's name. Local, like every generated name: a C string
    /// literal has no symbol at all, so no client can depend on this spelling.
    fn symbol(self) -> &'static str {
        match self {
            Msg::Assertion => "assertion_failed",
            Msg::CaseDefault => "case_no_matching_label",
            Msg::CaseType => "case_no_matching_type",
            Msg::Nil => "nil_dereference",
        }
    }
}

/// One runtime entry point the generated code calls, out of DESIGN §9.4's table:
/// the four the fragment's checks and traps need. Each is the object boundary's
/// entry point for a `static inline` helper of the same name in `oberon.h`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Rt {
    Index,
    Div,
    Mod,
    Trap,
    /// `rt_is(descriptor, target)`: whether an object is of a type (§9.3's
    /// `base` chain walk).
    Is,
    /// `rt_rec_copy(dst, src, n, self)`: a record copy that leaves the
    /// destination's own descriptor in place.
    RecCopy,
    /// `rt_new(size, descriptor)`: the only route to the heap, so the collector
    /// sees every object and a failed allocation is NIL (report §6.4).
    New,
    /// `ob_rt_guard_at(cond, at)`: a guard's report, whose byte is *in* the
    /// message the C back end bakes into a literal.
    GuardAt,
    /// `ob_gc_root_region(base, fields, n)`: a module variable the collector has
    /// to walk (DESIGN §15.4).
    GcRoot,
}

impl Rt {
    fn symbol(self) -> &'static str {
        match self {
            Rt::Index => "ob_rt_index_at",
            Rt::Div => "ob_rt_div_at",
            Rt::Mod => "ob_rt_mod_at",
            Rt::Trap => "ob_rt_trap_pos",
            Rt::Is => "ob_rt_is",
            Rt::RecCopy => "ob_rt_rec_copy",
            Rt::New => "rt_new",
            Rt::GuardAt => "ob_rt_guard_at",
            Rt::GcRoot => "ob_gc_root_region",
        }
    }
}

/// Lower one checked module to an object file, or say which construct it cannot
/// lower yet — as a diagnostic with that construct's span, so the driver prints
/// it like any other failure.
pub fn emit_object<'a>(
    module: &'a Module,
    types: &'a Types,
    sema: &'a Sema<'a>,
    checks: cgen::Checks,
) -> Result<Vec<u8>, Vec<Diagnostic>> {
    build(module, types, sema, checks).map_err(|d| vec![d])
}

fn build<'a>(
    module: &'a Module,
    types: &'a Types,
    sema: &'a Sema<'a>,
    checks: cgen::Checks,
) -> Result<Vec<u8>, Diagnostic> {
    let isa = host_isa()?;
    let obj = ObjectModule::new(
        ObjectBuilder::new(
            isa,
            module.name.clone(),
            cranelift_module::default_libcall_names(),
        )
        .map_err(|e| host(format!("{e}")))?,
    );
    let mut b = Backend::new(module, types, sema, checks, obj);
    b.run(module)?;
    b.obj
        .finish()
        .emit()
        .map_err(|e| host(format!("writing the object: {e}")))
}

/// The host's target ISA, with the optimisation level the design's comparison
/// asks for: `OptLevel` defaults to `none` (DESIGN §16), so an unoptimised back
/// end would have been measured against the C path's `-O2`.
///
/// The object path is ELF by construction: `ObjectBuilder` picks the binary
/// format from the ISA's triple, and the design's second back end is the ELF one.
/// A host whose format is something else is told so, rather than handed an object
/// nothing here was written for.
fn host_isa() -> Result<cranelift_codegen::isa::OwnedTargetIsa, Diagnostic> {
    let mut flags = settings::builder();
    flags
        .set("opt_level", "speed")
        .map_err(|e| host(format!("opt_level: {e}")))?;
    let isa_builder = cranelift_native::builder().map_err(|e| host(format!("the host's ISA: {e}")))?;
    if isa_builder.triple().binary_format != BinaryFormat::Elf {
        return Err(host(format!(
            "the clif back end emits ELF objects; this host's format is {} ({})",
            isa_builder.triple().binary_format,
            isa_builder.triple()
        )));
    }
    isa_builder
        .finish(settings::Flags::new(flags))
        .map_err(|e| host(format!("the target ISA did not configure: {e}")))
}

/// The target's pointer width in bytes, which is what a layout needs and what a
/// caller without an object module (`--layout`) has to ask for separately.
pub fn pointer_bytes() -> Result<u64, String> {
    let isa = host_isa().map_err(|d| d.message)?;
    Ok(u64::from(isa.frontend_config().pointer_bytes()))
}

/// Write a number into a data object's bytes: native endianness, and as wide as
/// `size` — the target is the host this back end was built on (the ELF check
/// above is what makes that explicit), so this is exactly what `cc` would have
/// put there, and it is the same width a `size_t` field has.
fn put_int(bytes: &mut [u8], at: usize, value: u64, size: u64) {
    let bytes_of = value.to_ne_bytes();
    let n = size as usize;
    let at = at.min(bytes.len().saturating_sub(1));
    let n = n.min(bytes.len() - at);
    bytes[at..at + n].copy_from_slice(&bytes_of[..n]);
}

/// A failure that is about this host rather than about the program. The span is
/// the file's first byte, which is what the driver's renderer expects.
fn host(message: String) -> Diagnostic {
    Diagnostic::error(Span::default(), message)
}

/// A construct this milestone's fragment does not cover. Naming the milestone is
/// what keeps it from reading as a limitation of the language, and the span makes
/// it the same kind of diagnostic as a type error: the reason and where.
fn not_yet(what: &str, span: Span) -> Diagnostic {
    Diagnostic::error(
        span,
        format!("the clif back end does not lower {what} yet (PLAN.md W9.2)"),
    )
}

struct Backend<'a> {
    /// The module, for the type declarations a label or a guard names.
    module: &'a Module,
    module_name: String,
    types: &'a Types,
    sema: &'a Sema<'a>,
    checks: cgen::Checks,
    obj: ObjectModule,
    /// What an address is in the IR, for the symbol loads and the runtime's
    /// pointer arguments, and how wide it is in bytes.
    ptr: ir::Type,
    ptr_bytes: u64,
    /// The C ABI's layout of every type the module stores: an array's element
    /// stride and (for records, PLAN.md W9.2b) a field's offset come from here.
    /// Computed once for the module rather than per body, because it is a
    /// property of the types and not of a function.
    layouts: HashMap<TypeId, Layout>,
    /// The module's record fields, for a `Field` selector's offset.
    records: HashMap<TypeId, crate::layout::RecordLayout>,
    /// The descriptor of every record type the module names (DESIGN §9.3), and
    /// the collector's field map of every one that reaches a pointer.
    descs: HashMap<TypeId, DataId>,
    maps: HashMap<TypeId, DataId>,
    nfields: HashMap<TypeId, u32>,
    /// A module variable's own map, when its type reaches a pointer: what
    /// `__init` registers with the collector.
    root_maps: HashMap<String, DataId>,
    /// Every procedure the module declares, under the symbol it is emitted with.
    procs: HashMap<String, FuncId>,
    /// The module's variables, as data objects.
    vars: HashMap<String, DataId>,
    /// What each module variable's type is: an index into one needs the length
    /// and the element type, which the data object does not carry.
    var_ty: HashMap<String, TypeId>,
    /// The runtime entry points and the registry call, declared for the module.
    rt: HashMap<Rt, FuncId>,
    register: FuncId,
    /// The messages this back end's own traps carry (`Msg`).
    msgs: HashMap<Msg, DataId>,
    /// The five fields below are filled by `run`, in the order it documents, and
    /// are read only after that: they start as ids that nothing can be, so a
    /// path that read one early would be a wrong symbol rather than a
    /// plausible-looking one.
    /// The module's own name, for `ob_register_commands`, and its command
    /// table (report §11).
    name_data: DataId,
    commands: DataId,
    /// The module body, which is what loading a module runs, and the entry point
    /// the driver's loader calls. Both are the module's interface (DESIGN §9.8),
    /// so both are exported.
    init: FuncId,
    load: FuncId,
}

impl<'a> Backend<'a> {
    fn new(
        module: &'a Module,
        types: &'a Types,
        sema: &'a Sema<'a>,
        checks: cgen::Checks,
        obj: ObjectModule,
    ) -> Backend<'a> {
        let ptr = obj.target_config().pointer_type();
        let ptr_bytes = u64::from(obj.target_config().pointer_bytes());
        Backend {
            module,
            module_name: module.name.clone(),
            types,
            sema,
            checks,
            obj,
            ptr,
            ptr_bytes,
            layouts: HashMap::new(),
            records: HashMap::new(),
            descs: HashMap::new(),
            maps: HashMap::new(),
            nfields: HashMap::new(),
            root_maps: HashMap::new(),
            procs: HashMap::new(),
            vars: HashMap::new(),
            var_ty: HashMap::new(),
            rt: HashMap::new(),
            register: FuncId::from_u32(0),
            msgs: HashMap::new(),
            name_data: DataId::from_u32(0),
            commands: DataId::from_u32(0),
            init: FuncId::from_u32(0),
            load: FuncId::from_u32(0),
        }
    }

    fn run(&mut self, module: &Module) -> Result<(), Diagnostic> {
        let span = module.span;
        // 0. The C ABI's layout for every type this module stores (DESIGN §16,
        //    `layout.rs`): the array strides below are read from it, as are the
        //    record member offsets W9.2's pointers and records will need.
        self.compute_layouts(module)?;
        // 1. The messages the back end carries itself: `ASSERT`'s, and the one
        //    an unmatched `CASE` reports (W2.5). Every other trap reports from
        //    the runtime helper that makes it. Both are declared whether or not
        //    the module can reach them, which is what the C back end does with
        //    its (usually empty) command table: a string that no call names is a
        //    few bytes of `.rodata`, and asking first would mean walking the
        //    whole body to answer a question with one byte of consequence.
        for msg in [Msg::Assertion, Msg::CaseDefault, Msg::CaseType, Msg::Nil] {
            let id = self.declare_string(msg.symbol(), msg.text())?;
            self.msgs.insert(msg, id);
        }
        // 2. The runtime entry points, and the registry. A declaration is a
        //    symbol-table entry and not code — the linker tolerates one that is
        //    never called, which the C path's mixture of modules already relies
        //    on — so they are declared once for the module; what the code
        //    *calls* is decided where it is lowered, and that is what reaches
        //    the object.
        for rt in [
            Rt::Index,
            Rt::Div,
            Rt::Mod,
            Rt::Trap,
            Rt::Is,
            Rt::RecCopy,
            Rt::New,
            Rt::GuardAt,
            Rt::GcRoot,
        ] {
            let sig = self.rt_signature(rt);
            let id = self
                .obj
                .declare_function(rt.symbol(), Linkage::Import, &sig)
                .map_err(|e| host(format!("{e}")))?;
            self.rt.insert(rt, id);
        }
        let mut register_sig = self.obj.make_signature();
        register_sig.params.push(AbiParam::new(self.ptr));
        register_sig.params.push(AbiParam::new(self.ptr));
        self.register = self
            .obj
            .declare_function("ob_register_commands", Linkage::Import, &register_sig)
            .map_err(|e| host(format!("{e}")))?;
        // 3. The module's storage, then its entry points, then its procedures,
        //    so that a body lowered later can name all of them.
        self.declare_module_data(module)?;
        // The module's record types, whose descriptors a heap object carries and
        // whose field maps the collector walks (§15.2), and then the variables'
        // own maps, which need those descriptors.
        self.declare_descriptors(module)?;
        self.name_data = self.declare_string("module_name", &module.name)?;
        self.init = self.declare_function(&self.symbol("init"), true, &[], None, span)?;
        self.load = self.declare_function(&self.symbol("load"), true, &[], None, span)?;
        for p in &module.decls.procs {
            self.declare_proc(p)?;
        }
        // 4. The command table, before any body is lowered: it names procedures
        //    that step 3 has declared, and `__load` (step 5's last) is what
        //    passes it to the registry.
        self.emit_commands(module)?;
        // 5. The bodies: the procedures, then the module body that loading runs,
        //    then the entry point that does the loading.
        for p in &module.decls.procs {
            self.lower_proc(p)?;
        }
        self.lower_function(self.init, &[], &[], None, &module.body, span)?;
        self.lower_load(module)?;
        Ok(())
    }

    // -------------------------------------------------------------- symbols

    /// Ask `layout.rs` for the layout of every type the module can store, so
    /// that the lowering reads a table rather than walking the type graph. The
    /// walk recurses through elements and bases, so the roots are the declared
    /// types and every variable, parameter and local.
    fn compute_layouts(&mut self, module: &Module) -> Result<(), Diagnostic> {
        let mut layouts = Layouts::new(self.types, self.ptr_bytes);
        for decl in &module.decls.types {
            if let Some(t) = self.sema.type_at(span_of_type(&decl.ty), self.types) {
                layouts.of(t);
            }
        }
        for v in &module.decls.vars {
            layouts.of(self.decl_type(&v.ty));
        }
        for p in &module.decls.procs {
            let (params, result) = self.signature_of(p)?;
            for (_, ty) in &params {
                layouts.of(*ty);
            }
            if let Some(r) = result {
                layouts.of(r);
            }
            for v in &p.decls.vars {
                layouts.of(self.decl_type(&v.ty));
            }
        }
        let tables = layouts.resolved();
        self.layouts = tables.layouts;
        self.records = tables.records;
        Ok(())
    }

    fn layout(&self, t: TypeId) -> Option<Layout> {
        self.layouts.get(&t).copied()
    }

    /// The dimensions of a fixed array, outermost first, and its element type.
    fn array_dims(&self, mut t: TypeId) -> Option<(Vec<u32>, TypeId)> {
        let mut dims = Vec::new();
        while let Type::Array { len, elem } = self.types.get(t) {
            dims.push(*len);
            t = *elem;
        }
        if dims.is_empty() {
            None
        } else {
            Some((dims, t))
        }
    }

    /// The number of elements a (possibly multi-dimensional) array has in all,
    /// and its innermost element type: the run the collector's map describes.
    fn array_run(&self, t: TypeId) -> (u64, TypeId) {
        match self.array_dims(t) {
            Some((dims, elem)) => (dims.iter().map(|n| u64::from(*n)).product(), elem),
            None => (1, t),
        }
    }

    /// One map entry: where the pointers are inside a region of this type whose
    /// first byte is at `off` (DESIGN §15.2, the C back end's `region_fields`).
    /// A pointer field is one slot; an array of pointers is a run of them; an
    /// array of records is a run of descriptor-fronted records, which their own
    /// map then walks; a record field is the run of one. Anything else — a word,
    /// a `CHAR`, a procedure value, which holds a code address and never an
    /// object — reaches no pointer, and a record that reaches none gets no map at
    /// all, so marking it costs one NULL test.
    fn region_fields(&self, t: TypeId, off: u64) -> Vec<(u64, u64, u64, Option<TypeId>)> {
        match self.types.get(t) {
            Type::Pointer { .. } => vec![(off, 0, 1, None)],
            Type::Array { .. } => {
                let (n, elem) = self.array_run(t);
                if !self.reaches_pointer(elem) {
                    return Vec::new();
                }
                match self.types.get(elem) {
                    Type::Pointer { .. } => vec![(off, self.ptr_bytes, n, None)],
                    Type::Record { .. } => vec![(
                        off,
                        self.layout(elem).map(|l| l.size).unwrap_or(0),
                        n,
                        Some(elem),
                    )],
                    _ => Vec::new(),
                }
            }
            Type::Record { .. } => {
                if !self.reaches_pointer(t) {
                    return Vec::new();
                }
                vec![(off, 0, 1, Some(t))]
            }
            _ => Vec::new(),
        }
    }

    /// The whole map of a record type: its base's fields first, then its own,
    /// each at the offset the layout gives (the C back end's
    /// `collect_record_fields`, whose `offsetof` terms are these numbers).
    fn record_map(&self, t: TypeId) -> Vec<(u64, u64, u64, Option<TypeId>)> {
        let mut out = Vec::new();
        self.collect_map(t, &mut out);
        out
    }

    fn collect_map(&self, t: TypeId, out: &mut Vec<(u64, u64, u64, Option<TypeId>)>) {
        let Some(r) = self.records.get(&t) else {
            return;
        };
        if let Some(b) = r.base {
            self.collect_map(b, out);
        }
        for (_, off, ty) in &r.fields {
            out.extend(self.region_fields(*ty, *off));
        }
    }

    /// Does a region of this type contain a record anywhere? What a descriptor
    /// initialisation has to walk (`desc_init`), since a nested record carries
    /// its own descriptor word.
    fn contains_record(&self, t: TypeId) -> bool {
        match self.types.get(t) {
            Type::Record { .. } => true,
            Type::Array { elem, .. } => self.contains_record(*elem),
            _ => false,
        }
    }

    /// The symbol a record type is emitted under, which is the C back end's C
    /// struct name: a declared type keeps its Oberon name, and a composite
    /// reached through a pointer without a declaration of its own gets a
    /// deterministic one (the C emitter numbers anonymous records as it meets
    /// them, which two back ends cannot agree on — a name no client can write is
    /// the one case where that matters).
    fn type_symbol(&self, t: TypeId, names: &HashMap<TypeId, String>) -> String {
        names
            .get(&t)
            .cloned()
            .unwrap_or_else(|| format!("{}__Anon_{}", self.module_name, t.0))
    }

    /// The data the descriptor machinery needs, for every record type the module
    /// can name: the type's field map, the name it carries, and the descriptor
    /// itself (`ObDesc`, DESIGN §9.3). The C back end writes the same three
    /// things as C text (`emit_type`); here they are data objects, and the
    /// collector walks the map exactly as it walks the C one — which is why a
    /// mixed build's heap is one heap.
    fn declare_descriptors(&mut self, module: &Module) -> Result<(), Diagnostic> {
        let mut names: HashMap<TypeId, String> = HashMap::new();
        for decl in &module.decls.types {
            if let Some(t) = self.sema.type_at(span_of_type(&decl.ty), self.types) {
                names.insert(t, format!("{}__{}", self.module_name, mangle(&decl.name)));
            }
        }
        let mut records: Vec<TypeId> = self.records.keys().copied().collect();
        records.sort_by_key(|t| t.0);
        // Declare first: a derived record's descriptor names its base's, and a
        // map names the descriptors of the records inside it.
        for &t in &records {
            let symbol = self.type_symbol(t, &names);
            let entries = self.record_map(t);
            if !entries.is_empty() {
                let id = self
                    .obj
                    .declare_data(
                        &format!("{symbol}__fields"),
                        Linkage::Local,
                        false,
                        false,
                    )
                    .map_err(|e| host(format!("{e}")))?;
                self.maps.insert(t, id);
                self.nfields.insert(t, entries.len() as u32);
            }
            let id = self
                .obj
                .declare_data(&format!("TD_{symbol}"), Linkage::Export, false, false)
                .map_err(|e| host(format!("{e}")))?;
            self.descs.insert(t, id);
        }
        // The maps: their entries are numbers, and the descriptors they name are
        // relocations into them.
        let fld = crate::layout::ob_fld(self.ptr_bytes);
        for &t in &records {
            let Some(id) = self.maps.get(&t).copied() else {
                continue;
            };
            let entries = self.record_map(t);
            let mut bytes = vec![0u8; entries.len() * fld.size as usize];
            for (i, (off, stride, n, _)) in entries.iter().enumerate() {
                let at = i * fld.size as usize;
                put_int(&mut bytes, at + fld.off as usize, *off, self.ptr_bytes);
                put_int(&mut bytes, at + fld.stride as usize, *stride, self.ptr_bytes);
                put_int(&mut bytes, at + fld.n as usize, *n, self.ptr_bytes);
            }
            let mut data = DataDescription::new();
            data.define(bytes.into_boxed_slice());
            data.set_align(self.ptr_bytes);
            for (i, (_, _, _, inner)) in entries.iter().enumerate() {
                if let Some(inner) = inner {
                    let at = (i as u64 * fld.size + fld.desc) as u32;
                    let gv = self.obj.declare_data_in_data(self.descs[inner], &mut data);
                    data.write_data_addr(at, gv, 0);
                }
            }
            self.obj
                .define_data(id, &data)
                .map_err(|e| host(format!("{e}")))?;
        }
        // The descriptors, in id order, with their bases' already declared.
        for &t in &records {
            let symbol = self.type_symbol(t, &names);
            let name_id = self.declare_string(&format!("type_name_{}", t.0), &symbol)?;
            let d = crate::layout::ob_desc(self.ptr_bytes);
            let mut bytes = vec![0u8; d.size as usize];
            put_int(&mut bytes, d.id as usize, u64::from(t.0), self.ptr_bytes);
            put_int(
                &mut bytes,
                d.nfields as usize,
                u64::from(self.nfields.get(&t).copied().unwrap_or(0)),
                self.ptr_bytes,
            );
            let mut data = DataDescription::new();
            data.define(bytes.into_boxed_slice());
            data.set_align(self.ptr_bytes);
            if let Some(b) = self.records.get(&t).and_then(|r| r.base) {
                if let Some(bid) = self.descs.get(&b).copied() {
                    let gv = self.obj.declare_data_in_data(bid, &mut data);
                    data.write_data_addr(d.base as u32, gv, 0);
                }
            }
            let gv = self.obj.declare_data_in_data(name_id, &mut data);
            data.write_data_addr(d.name as u32, gv, 0);
            if let Some(mid) = self.maps.get(&t).copied() {
                let gv = self.obj.declare_data_in_data(mid, &mut data);
                data.write_data_addr(d.fields as u32, gv, 0);
            }
            let id = self.descs[&t];
            self.obj
                .define_data(id, &data)
                .map_err(|e| host(format!("{e}")))?;
        }
        // The module variables' own maps, which is what the collector walks for
        // a root that is not on the stack (DESIGN §15.4) — the C back end's
        // `<var>__roots[]`.
        for (name, id) in self.root_maps.clone() {
            let ty = self.var_ty[&name];
            let entries = self.region_fields(ty, 0);
            let mut bytes = vec![0u8; entries.len() * fld.size as usize];
            for (i, (off, stride, n, _)) in entries.iter().enumerate() {
                let at = i * fld.size as usize;
                put_int(&mut bytes, at + fld.off as usize, *off, self.ptr_bytes);
                put_int(&mut bytes, at + fld.stride as usize, *stride, self.ptr_bytes);
                put_int(&mut bytes, at + fld.n as usize, *n, self.ptr_bytes);
            }
            let mut data = DataDescription::new();
            data.define(bytes.into_boxed_slice());
            data.set_align(self.ptr_bytes);
            for (i, (_, _, _, inner)) in entries.iter().enumerate() {
                if let Some(inner) = inner {
                    let at = (i as u64 * fld.size + fld.desc) as u32;
                    let gv = self.obj.declare_data_in_data(self.descs[inner], &mut data);
                    data.write_data_addr(at, gv, 0);
                }
            }
            self.obj
                .define_data(id, &data)
                .map_err(|e| host(format!("{e}")))?;
        }
        Ok(())
    }

    /// The name a module-level Oberon object is emitted under. The same rule as
    /// the C back end's, C-keyword escape included, because a client compiled
    /// against the module's `.h` looks for exactly this symbol.
    fn symbol(&self, name: &str) -> String {
        format!("{}__{}", self.module_name, mangle(name))
    }

    fn rt_signature(&self, rt: Rt) -> Signature {
        let mut sig = self.obj.make_signature();
        match rt {
            // `rt_is(const ObDesc *, const ObDesc *) -> int32_t`.
            Rt::Is => {
                sig.params.push(AbiParam::new(self.ptr));
                sig.params.push(AbiParam::new(self.ptr));
                sig.returns.push(AbiParam::new(I32));
            }
            // `rt_rec_copy(void *, const void *, int32_t, const ObDesc *)`.
            Rt::RecCopy => {
                sig.params.push(AbiParam::new(self.ptr));
                sig.params.push(AbiParam::new(self.ptr));
                sig.params.push(AbiParam::new(I32));
                sig.params.push(AbiParam::new(self.ptr));
            }
            // `rt_new(size_t, const ObDesc *) -> void *`, whose size is as wide
            // as the host's `size_t` (R30).
            Rt::New => {
                sig.params.push(AbiParam::new(self.ptr));
                sig.params.push(AbiParam::new(self.ptr));
                sig.returns.push(AbiParam::new(self.ptr));
            }
            // `ob_rt_guard_at(int32_t cond, int32_t at)`.
            Rt::GuardAt => {
                sig.params.push(AbiParam::new(I32));
                sig.params.push(AbiParam::new(I32));
            }
            // `ob_gc_root_region(const void *, const ObFld *, int32_t)`.
            Rt::GcRoot => {
                sig.params.push(AbiParam::new(self.ptr));
                sig.params.push(AbiParam::new(self.ptr));
                sig.params.push(AbiParam::new(I32));
            }
            // `rt_trap_pos(const char *, int32_t)`: the message, then the byte.
            Rt::Trap => {
                sig.params.push(AbiParam::new(self.ptr));
                sig.params.push(AbiParam::new(I32));
            }
            // The three checks take (value, limit, byte) and answer with the
            // value (DESIGN §9.4).
            Rt::Index | Rt::Div | Rt::Mod => {
                for _ in 0..3 {
                    sig.params.push(AbiParam::new(I32));
                }
                sig.returns.push(AbiParam::new(I32));
            }
        }
        sig
    }

    fn declare_function(
        &mut self,
        name: &str,
        export: bool,
        params: &[TypeId],
        result: Option<TypeId>,
        span: Span,
    ) -> Result<FuncId, Diagnostic> {
        let mut sig = self.obj.make_signature();
        for p in params {
            let ty = self.scalar(*p, span)?;
            sig.params.push(AbiParam::new(ty));
        }
        if let Some(r) = result {
            sig.returns.push(AbiParam::new(self.scalar(r, span)?));
        }
        let linkage = if export {
            Linkage::Export
        } else {
            Linkage::Local
        };
        self.obj
            .declare_function(name, linkage, &sig)
            .map_err(|e| host(format!("{e}")))
    }

    /// The one type decision this back end makes: an `INTEGER` (and a
    /// `BOOLEAN`, which the C back end emits as an `int32_t` holding 0 or 1) is
    /// the IR's `i32`, a pointer is the target's address width. Everything else
    /// is refused *here*, where the program's own naming can be used, rather
    /// than somewhere in the lowering where it would read as an internal
    /// failure.
    fn scalar(&self, t: TypeId, span: Span) -> Result<ir::Type, Diagnostic> {
        match self.value_type(t) {
            Some(ty) => Ok(ty),
            None => Err(not_yet(
                &format!("a value of type {}", type_name(self.types.get(t))),
                span,
            )),
        }
    }

    /// A type's alignment, from the layout: what a frame slot holding one has to
    /// be aligned to (the same number the C back end's `memset` of a local gets
    /// from the C compiler).
    fn type_align(&self, t: TypeId) -> u64 {
        self.layout(t).map(|l| l.align).unwrap_or(1)
    }

    /// The IR type a *value* of this Oberon type is held in, or `None` for a
    /// type that is a region rather than a value (a record, an array) or one
    /// this slice does not lower yet (`REAL`, `CHAR`, `SET`, PLAN.md W9.2c).
    fn value_type(&self, t: TypeId) -> Option<ir::Type> {
        match self.types.get(t) {
            Type::Basic(Basic::Integer) | Type::Basic(Basic::Boolean) => Some(I32),
            Type::Pointer { .. } => Some(self.ptr),
            _ => None,
        }
    }

    /// What a variable or a field of this type occupies, or a refusal. A record
    /// and an array are regions: their size is the layout's, and their *values*
    /// are not held in anything (a copy is `rt_rec_copy`, PLAN.md W9.2b's).
    fn storable(&self, t: TypeId, span: Span) -> Result<u64, Diagnostic> {
        match self.types.get(t) {
            Type::Basic(Basic::Integer) | Type::Basic(Basic::Boolean) => Ok(4),
            Type::Pointer { .. } => Ok(self.ptr_bytes),
            Type::Record { .. } | Type::Array { .. } => match self.layout(t) {
                Some(l) => Ok(l.size),
                None => Err(not_yet(&format!("a value of type {}", type_name(self.types.get(t))), span)),
            },
            other => Err(not_yet(
                &format!("a variable of type {}", type_name(other)),
                span,
            )),
        }
    }

    /// Does a region of this type reach a pointer at all? The collector's answer
    /// to "is this worth a map": a record that reaches none gets none, so marking
    /// it costs one NULL test (DESIGN §15.2). A pointer ends the walk, so a type
    /// cannot recurse forever.
    fn reaches_pointer(&self, t: TypeId) -> bool {
        match self.types.get(t) {
            Type::Pointer { .. } => true,
            Type::Array { elem, .. } => self.reaches_pointer(*elem),
            Type::Record { base, fields, .. } => {
                base.map(|b| self.reaches_pointer(b)).unwrap_or(false)
                    || fields.iter().any(|f| self.reaches_pointer(f.ty))
            }
            _ => false,
        }
    }

    /// A record's own field, at the offset a `Field` selector adds: the base
    /// chain is walked, because an inherited field lives in the base's storage
    /// and the derived record embeds it first (so the offsets *within* the
    /// embedded base are the base's own).
    fn field_of(&self, rec: TypeId, name: &str) -> Option<(u64, TypeId)> {
        let mut cur = rec;
        let mut at = 0;
        for _ in 0..64 {
            let r = self.records.get(&cur)?;
            if let Some((_, off, ty)) = r.fields.iter().find(|(n, _, _)| n == name) {
                return Some((at + off, *ty));
            }
            at += 0; // the base is the first member: its fields start where it does
            cur = r.base?;
        }
        None
    }

    /// The descriptor of a record type, by the same symbol the C back end gives
    /// it (`TD_<Module>__<Name>`, DESIGN §9.3), or a refusal for a type the
    /// module has no descriptor for.
    fn descriptor(&self, t: TypeId) -> Option<DataId> {
        self.descs.get(&t).copied()
    }

    /// Read-only data for a string the runtime reads: a trap message, or the
    /// module's name for the registry. The C back end writes these as literals,
    /// which have no symbol; a lowered function can only name a symbol, so they
    /// are data objects here.
    fn declare_string(&mut self, what: &str, text: &str) -> Result<DataId, Diagnostic> {
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(0);
        let mut data = DataDescription::new();
        data.define(bytes.into_boxed_slice());
        data.set_align(1);
        let id = self
            .obj
            .declare_data(&self.symbol(what), Linkage::Local, false, false)
            .map_err(|e| host(format!("{e}")))?;
        self.obj
            .define_data(id, &data)
            .map_err(|e| host(format!("{e}")))?;
        Ok(id)
    }

    fn declare_module_data(&mut self, module: &Module) -> Result<(), Diagnostic> {
        for v in &module.decls.vars {
            let ty = self.decl_type(&v.ty);
            let size = self.data_size(ty, v.span)?;
            for (name, export) in &v.names {
                let symbol = self.symbol(name);
                let linkage = if *export {
                    Linkage::Export
                } else {
                    Linkage::Local
                };
                let id = self
                    .obj
                    .declare_data(&symbol, linkage, true, false)
                    .map_err(|e| host(format!("{e}")))?;
                let mut data = DataDescription::new();
                // Zero-initialised, as the C back end's statics are: both paths
                // then read zero from a variable that was never assigned, which
                // is the one value the report leaves free.
                data.define_zeroinit(size);
                // Aligned as the *type* needs, not as a word: a variable that
                // holds a pointer — a `GCPtr`, a record with one, an array of
                // either — is what the collector reads through, and a misaligned
                // one is a fault in its walk, not in the program (the C back end
                // gets this from the type it declares).
                data.set_align(self.layout(ty).map(|l| l.align).unwrap_or(WORD));
                self.obj
                    .define_data(id, &data)
                    .map_err(|e| host(format!("{e}")))?;
                self.vars.insert(name.clone(), id);
                self.var_ty.insert(name.clone(), ty);
                // The collector's root, registered before the body can allocate
                // (§15.4), and only when the variable's region can hold a
                // pointer at all.
                if self.reaches_pointer(ty) {
                    let rid = self
                        .obj
                        .declare_data(
                            &format!("{symbol}__roots"),
                            Linkage::Local,
                            false,
                            false,
                        )
                        .map_err(|e| host(format!("{e}")))?;
                    self.root_maps.insert(name.clone(), rid);
                }
            }
        }
        Ok(())
    }

    /// What a variable of this type holds, from the layout table, or a refusal:
    /// the same rule as `storable`, with the variable's own span in the message.
    fn data_size(&self, t: TypeId, span: Span) -> Result<usize, Diagnostic> {
        Ok(self.storable(t, span)? as usize)
    }

    fn declare_proc(&mut self, p: &Proc) -> Result<(), Diagnostic> {
        // A nested procedure reaches its enclosing frame through a static link
        // (DESIGN §9.7), which this back end has no frames for yet.
        if let Some(inner) = p.decls.procs.first() {
            return Err(not_yet("a nested procedure", inner.name_span));
        }
        // The checker's flattened signature is what names the parameters, and
        // it is what the C back end reads for the same purpose.
        let (params, result) = self.signature_of(p)?;
        let types: Vec<TypeId> = params.iter().map(|(_, ty)| *ty).collect();
        let id = self.declare_function(&self.symbol(&p.name), p.export, &types, result, p.name_span)?;
        self.procs.insert(p.name.clone(), id);
        Ok(())
    }

    /// A procedure's parameters, as the checker flattened them, and its result.
    /// A `VAR` parameter needs an address in the IR and a pointer on the C side
    /// (report §10.1), which is PLAN.md W9.2's.
    fn signature_of(&self, p: &Proc) -> Result<(Vec<Formal>, Option<TypeId>), Diagnostic> {
        let sig = self
            .sema
            .signatures
            .get(&span_key(p.span))
            .ok_or_else(|| host(format!("no signature for `{}`", p.name)))?;
        let mut params = Vec::new();
        for (name, pm) in &sig.params {
            if pm.by_var {
                return Err(not_yet(
                    &format!("the VAR parameter `{name}`"),
                    p.name_span,
                ));
            }
            params.push((name.clone(), pm.ty));
        }
        Ok((params, sig.result))
    }

    // ---------------------------------------------------------- entry points

    /// `Module__load`: run the module body, then register its commands, which is
    /// what report §11 says loading a module does and what the driver's loader
    /// has always called (DESIGN §9.8).
    fn lower_load(&mut self, _module: &Module) -> Result<(), Diagnostic> {
        let id = self.load;
        let init = self.init;
        let register = self.register;
        let name_data = self.name_data;
        let commands = self.commands;

        let mut ctx = self.obj.make_context();
        let sig = self.obj.make_signature();
        ctx.func = ir::Function::with_name_signature(UserFuncName::user(0, id.as_u32()), sig);
        let init_ref = self.obj.declare_func_in_func(init, &mut ctx.func);
        let register_ref = self.obj.declare_func_in_func(register, &mut ctx.func);
        let name_gv = self.obj.declare_data_in_func(name_data, &mut ctx.func);
        let table_gv = self.obj.declare_data_in_func(commands, &mut ctx.func);
        let mut fn_ctx = FunctionBuilderContext::new();
        {
            let mut bcx = FunctionBuilder::new(&mut ctx.func, &mut fn_ctx);
            let entry = bcx.create_block();
            bcx.append_block_params_for_function_params(entry);
            bcx.switch_to_block(entry);
            bcx.ins().call(init_ref, &[]);
            let table = bcx.ins().symbol_value(self.ptr, table_gv);
            let module = bcx.ins().symbol_value(self.ptr, name_gv);
            bcx.ins().call(register_ref, &[module, table]);
            bcx.ins().return_(&[]);
            bcx.seal_all_blocks();
            bcx.finalize(self.obj.target_config());
        }
        // A codegen failure here is a bug in *this* file — the module checked
        // and the lowering is what wrote the IR — and the driver is handed the
        // Debug form, because the `Display` of a verifier failure is the words
        // "Verifier errors" and nothing else: the instruction index and the type
        // it got wrong are the diagnosis.
        self.obj
            .define_function(id, &mut ctx)
            .map_err(|e| host(format!("{e:?}")))
    }

    /// The command table of report §11: one entry per *parameterless exported*
    /// procedure, then a NULL name to end it. A parameterized procedure is not a
    /// command — calling one through a `void (*)(void)` would be undefined —
    /// which is the rule the C back end applies too (DESIGN §9.7).
    fn emit_commands(&mut self, module: &Module) -> Result<(), Diagnostic> {
        let mut entries: Vec<(String, FuncId)> = Vec::new();
        for p in &module.decls.procs {
            if p.export && p.params.is_empty() && p.result.is_none() {
                let id = *self
                    .procs
                    .get(&p.name)
                    .expect("a declared procedure has a symbol");
                entries.push((p.name.clone(), id));
            }
        }
        let psize = self.obj.target_config().pointer_bytes() as usize;
        let mut table = DataDescription::new();
        // Zeroes with *contents*, not `define_zeroinit`: a table of addresses
        // carries relocations, and a section with no bytes in the file (`.bss`)
        // is one a static link cannot write them into — the loader then reads a
        // table of nulls and no command is found.
        table.define(vec![0u8; (entries.len() + 1) * 2 * psize].into_boxed_slice());
        table.set_align(psize as u64);
        for (i, (name, id)) in entries.iter().enumerate() {
            let offset = (i * 2 * psize) as u32;
            let name_id = self.declare_string(&format!("command_name_{i}"), name)?;
            let gv = self.obj.declare_data_in_data(name_id, &mut table);
            table.write_data_addr(offset, gv, 0);
            let fref = self.obj.declare_func_in_data(*id, &mut table);
            table.write_function_addr(offset + psize as u32, fref);
        }
        let symbol = self.symbol("commands");
        let id = self
            .obj
            .declare_data(&symbol, Linkage::Local, false, false)
            .map_err(|e| host(format!("{e}")))?;
        self.obj
            .define_data(id, &table)
            .map_err(|e| host(format!("{e}")))?;
        self.commands = id;
        Ok(())
    }

    // ----------------------------------------------------------- functions

    fn lower_proc(&mut self, p: &Proc) -> Result<(), Diagnostic> {
        let (params, result) = self.signature_of(p)?;
        let mut locals = Vec::new();
        for v in &p.decls.vars {
            let ty = self.decl_type(&v.ty);
            for (lname, _) in &v.names {
                locals.push((lname.clone(), ty));
            }
        }
        let id = *self
            .procs
            .get(&p.name)
            .expect("a declared procedure has a symbol");
        let body = p.body.clone();
        self.lower_function(id, &params, &locals, result, &body, p.name_span)
    }

    /// Lower one function: parameters and locals as SSA variables, the body as
    /// IR, and a path that runs off the end returning.
    fn lower_function(
        &mut self,
        id: FuncId,
        params: &[(String, TypeId)],
        locals: &[(String, TypeId)],
        result: Option<TypeId>,
        body: &[Stmt],
        span: Span,
    ) -> Result<(), Diagnostic> {
        // Every symbol the body can name is declared *before* the builder
        // exists: a Cranelift function reference is part of the function, and
        // the frontend does not hand the `ir::Function` back once it holds it.
        let mut ctx = self.obj.make_context();
        let mut sig = self.obj.make_signature();
        for (_, ty) in params {
            sig.params
                .push(AbiParam::new(self.scalar(*ty, span)?));
        }
        if let Some(r) = result {
            sig.returns.push(AbiParam::new(self.scalar(r, span)?));
        }
        ctx.func = ir::Function::with_name_signature(UserFuncName::user(0, id.as_u32()), sig);

        let mut frefs = HashMap::new();
        for (pname, pid) in self.procs.clone() {
            frefs.insert(pname, self.obj.declare_func_in_func(pid, &mut ctx.func));
        }
        let mut gvs = HashMap::new();
        for (vname, vid) in self.vars.clone() {
            gvs.insert(vname, self.obj.declare_data_in_func(vid, &mut ctx.func));
        }
        let mut rts = HashMap::new();
        for (rt, rid) in self.rt.clone() {
            rts.insert(rt, self.obj.declare_func_in_func(rid, &mut ctx.func));
        }
        let mut msgs = HashMap::new();
        for (msg, mid) in self.msgs.clone() {
            msgs.insert(msg, self.obj.declare_data_in_func(mid, &mut ctx.func));
        }
        let mut descs = HashMap::new();
        for (t, did) in self.descs.clone() {
            descs.insert(t, self.obj.declare_data_in_func(did, &mut ctx.func));
        }
        let mut roots = HashMap::new();
        for (name, mid) in self.root_maps.clone() {
            let n = self.region_fields(self.var_ty[&name], 0).len() as i32;
            roots.insert(
                name,
                (
                    self.obj.declare_data_in_func(mid, &mut ctx.func),
                    n,
                ),
            );
        }

        let mut fn_ctx = FunctionBuilderContext::new();
        {
            let mut bcx = FunctionBuilder::new(&mut ctx.func, &mut fn_ctx);
            let entry = bcx.create_block();
            bcx.append_block_params_for_function_params(entry);
            bcx.switch_to_block(entry);
            let mut vars = HashMap::new();
            let mut local_tys: HashMap<String, TypeId> = HashMap::new();
            let values: Vec<ir::Value> = bcx.block_params(entry).to_vec();
            for (i, (pname, pty)) in params.iter().enumerate() {
                let var = bcx.declare_var(self.scalar(*pty, span)?);
                bcx.def_var(var, values[i]);
                vars.insert(pname.clone(), var);
                local_tys.insert(pname.clone(), *pty);
            }
            let mut slots: HashMap<String, ir::Value> = HashMap::new();
            let mut regions: Vec<(TypeId, ir::Value)> = Vec::new();
            for (lname, ty) in locals {
                local_tys.insert(lname.clone(), *ty);
                if self.value_type(*ty).is_none() {
                    // A region — a record or an array — lives in a frame slot,
                    // zeroed exactly as the C back end zeroes a local (`memset`)
                    // and given its descriptor just after (a record carries one,
                    // DESIGN §9.3).
                    let size = self.layout(*ty).map(|l| l.size).ok_or_else(|| {
                        not_yet(&format!("the local `{lname}`"), span)
                    })?;
                    if size > u64::from(u32::MAX) {
                        // A frame slot's size is a 32-bit field in Cranelift's
                        // IR; the C path's local is a `sizeof`-sized object.
                        return Err(Diagnostic::error(
                            span,
                            format!(
                                "the local `{lname}` is {size} bytes; the clif back end's stack \
                                 slots are 32-bit"
                            ),
                        ));
                    }
                    let align = self
                        .layout(*ty)
                        .map(|_| self.type_align(*ty))
                        .unwrap_or(1)
                        .trailing_zeros() as u8;
                    let slot = bcx.create_sized_stack_slot(StackSlotData::new(
                        StackSlotKind::ExplicitSlot,
                        size as u32,
                        align,
                    ));
                    let addr = bcx.ins().stack_addr(self.ptr, slot, 0);
                    bcx.emit_small_memset(
                        self.obj.target_config(),
                        addr,
                        0,
                        size,
                        align,
                        ir::MemFlagsData::new(),
                    );
                    slots.insert(lname.clone(), addr);
                    if self.contains_record(*ty) {
                        regions.push((*ty, addr));
                    }
                } else {
                    let var = bcx.declare_var(self.scalar(*ty, span)?);
                    vars.insert(lname.clone(), var);
                }
            }
            let mut l = Lower::new(
                self, bcx, vars, slots, local_tys, gvs, frefs, rts, msgs, descs, roots, result,
            );
            for (ty, addr) in regions {
                l.desc_init(ty, addr, span)?;
            }
            // A module body registers its variables' roots before anything can
            // allocate (DESIGN §15.4), and gives every record variable its
            // descriptor — the C back end's `__init` preamble.
            if id == self.init {
                l.init_preamble(span)?;
            }
            l.stmts(body)?;
            // A path that reaches the end of a body returns zero: the C back
            // end zero-initialises its result accumulator for the same reason,
            // since a function the checker accepted may reach no `RETURN` at all
            // (W8.4) and the report defines no value for that path.
            if !l.ended() {
                if result.is_some() {
                    let zero = l.bcx.ins().iconst(I32, 0);
                    l.bcx.ins().return_(&[zero]);
                } else {
                    l.bcx.ins().return_(&[]);
                }
            }
            // Sealing at the end rather than block by block is the frontend's
            // own lazy mode, which is what a lowering that reaches a loop's
            // header after the back edge, or an `IF`'s join after its arms,
            // wants: the SSA construction then resolves the block arguments in
            // one pass instead of guessing at each branch.
            l.bcx.seal_all_blocks();
            l.bcx.finalize(l.b.obj.target_config());
        }
        // A codegen failure here is a bug in *this* file — the module checked
        // and the lowering is what wrote the IR — and the driver is handed the
        // Debug form, because the `Display` of a verifier failure is the words
        // "Verifier errors" and nothing else: the instruction index and the type
        // it got wrong are the diagnosis.
        self.obj
            .define_function(id, &mut ctx)
            .map_err(|e| host(format!("{e:?}")))
    }

    /// The type a declaration's `TypeExpr` denotes: the semantic pass recorded
    /// every type expression by span, and this reads it the same way the C back
    /// end does.
    fn decl_type(&self, t: &crate::ast::TypeExpr) -> TypeId {
        self.sema
            .type_at(span_of_type(t), self.types)
            .unwrap_or(TypeId(0))
    }
}

/// A type's name as a refusal reads with. Not shared with the C back end, whose
/// `c_type` answers a different question: this one is a diagnosis.
fn type_name(t: &Type) -> &'static str {
    match t {
        Type::Basic(Basic::Byte) => "BYTE",
        Type::Basic(Basic::Char) => "CHAR",
        Type::Basic(Basic::Real) => "REAL",
        Type::Basic(Basic::Set) => "SET",
        Type::Array { .. } => "an ARRAY",
        Type::OpenArray { .. } => "an open array",
        Type::Record { .. } => "a RECORD",
        Type::Pointer { .. } => "a POINTER",
        Type::Proc { .. } => "a PROCEDURE type",
        Type::Nil => "NIL",
        Type::StringLit(_) => "a string literal",
        Type::Basic(Basic::Integer) | Type::Basic(Basic::Boolean) | Type::Error => "an error type",
    }
}

/// Where a designator lives: a local that holds a *value* is an SSA variable
/// (which is what a register is), and anything else is an address — a module
/// variable's data object, a local record's or array's frame slot, a field or an
/// element of one, or a heap object behind a pointer.
enum Place {
    Local(Variable, TypeId),
    Addr(ir::Value, TypeId),
}

fn place_ty(place: &Place) -> TypeId {
    match place {
        Place::Local(_, t) | Place::Addr(_, t) => *t,
    }
}

/// The IR for one procedure: SSA values for its locals, a data object for each
/// module variable, and the references its body may name.
struct Lower<'b, 'f, 'a> {
    b: &'b Backend<'a>,
    bcx: FunctionBuilder<'f>,
    vars: HashMap<String, Variable>,
    /// A local whose type is an array: its storage is a zeroed stack slot, and
    /// what the body names is the address (cgen zeroes a local the same way).
    slots: HashMap<String, ir::Value>,
    gv: HashMap<String, ir::GlobalValue>,
    frefs: HashMap<String, ir::FuncRef>,
    rt: HashMap<Rt, ir::FuncRef>,
    msgs: HashMap<Msg, ir::GlobalValue>,
    /// The descriptor of every record type the module names, as a symbol the
    /// body can take the address of.
    descs: HashMap<TypeId, ir::GlobalValue>,
    /// A module variable's collector map, for `__init`'s registrations.
    roots: HashMap<String, (ir::GlobalValue, i32)>,
    /// The type of every local, a value local or a region one.
    local_tys: HashMap<String, TypeId>,
    /// The narrowing a type-`CASE` arm gives the case variable (report §9.5).
    narrow: Vec<HashMap<String, TypeId>>,
    /// The result type, which is what `RETURN` answers with.
    result: Option<TypeId>,
    /// The loops an `EXIT` leaves, innermost last.
    loops: Vec<ir::Block>,
}

impl<'b, 'f, 'a> Lower<'b, 'f, 'a> {
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn new(
        b: &'b Backend<'a>,
        bcx: FunctionBuilder<'f>,
        vars: HashMap<String, Variable>,
        slots: HashMap<String, ir::Value>,
        local_tys: HashMap<String, TypeId>,
        gv: HashMap<String, ir::GlobalValue>,
        frefs: HashMap<String, ir::FuncRef>,
        rt: HashMap<Rt, ir::FuncRef>,
        msgs: HashMap<Msg, ir::GlobalValue>,
        descs: HashMap<TypeId, ir::GlobalValue>,
        roots: HashMap<String, (ir::GlobalValue, i32)>,
        result: Option<TypeId>,
    ) -> Lower<'b, 'f, 'a> {
        Lower {
            b,
            bcx,
            vars,
            slots,
            gv,
            frefs,
            rt,
            msgs,
            descs,
            roots,
            local_tys,
            narrow: Vec::new(),
            result,
            loops: Vec::new(),
        }
    }

    // ------------------------------------------------------------ statements

    fn stmts(&mut self, body: &[Stmt]) -> Result<(), Diagnostic> {
        for s in body {
            // A statement after a `RETURN` in the same block is unreachable, and
            // the IR cannot hold it: the rest of the sequence is not emitted,
            // exactly as a C compiler drops it after a `return`.
            if self.ended() {
                return Ok(());
            }
            self.stmt(s)?;
        }
        Ok(())
    }

    fn stmt(&mut self, s: &Stmt) -> Result<(), Diagnostic> {
        match &s.kind {
            StmtKind::Assign { target, value } => self.assign(target, value),
            StmtKind::Call(d) => self.call_stmt(d),
            StmtKind::If { arms, els } => self.if_stmt(arms, els.as_deref()),
            StmtKind::While { arms } => self.while_stmt(arms),
            StmtKind::Repeat { body, until } => self.repeat_stmt(body, until),
            StmtKind::For {
                var,
                from,
                to,
                by,
                body,
            } => self.for_stmt(var, from, to, by.as_ref(), body, s.span),
            StmtKind::Case { subject, arms } => self.case_stmt(subject, arms, s.span),
            StmtKind::Exit => {
                let target = *self
                    .loops
                    .last()
                    .ok_or_else(|| not_yet("EXIT outside a loop", s.span))?;
                self.bcx.ins().jump(target, &[]);
                Ok(())
            }
            StmtKind::Return(e) => {
                match (self.result, e) {
                    (Some(_), Some(e)) => {
                        let v = self.expr(e)?;
                        self.bcx.ins().return_(&[v]);
                    }
                    // Unreachable for a module that checks: the checker reports
                    // a value returned by a proper procedure and a function that
                    // returns none. Both leave a well-formed signature anyway.
                    (Some(_), None) => {
                        let zero = self.bcx.ins().iconst(I32, 0);
                        self.bcx.ins().return_(&[zero]);
                    }
                    (None, _) => {
                        self.bcx.ins().return_(&[]);
                    }
                }
                Ok(())
            }
        }
    }

    /// `d := e`. The value is lowered first and the place second, so a program
    /// whose two sides could both trap reports the value's trap: C leaves that
    /// order to the compiler (R23), and the IR has an order of its own.
    fn assign(&mut self, target: &Designator, value: &Expr) -> Result<(), Diagnostic> {
        let place = self.place(target, target.span)?;
        let ty = place_ty(&place);
        // A record is a region, not a value: assigning one is a copy, and
        // `rt_rec_copy` is the call the C back end makes for an up-cast — it
        // moves the fields and leaves the destination's own descriptor in place,
        // which is what keeps a base variable from advertising a derived dynamic
        // type (DESIGN §9.3).
        if matches!(self.b.types.get(ty), Type::Record { .. }) {
            let src = match &value.kind {
                ExprKind::Designator(d) => self.place(d, value.span)?,
                _ => {
                    return Err(not_yet(
                        "a record assigned from something that is not a designator",
                        value.span,
                    ))
                }
            };
            let (dst, _) = self.address_of(&place, target.span)?;
            let (from, _) = self.address_of(&src, value.span)?;
            let size = self.b.layout(ty).map(|l| l.size).ok_or_else(|| {
                not_yet(
                    &format!("a copy of {}", type_name(self.b.types.get(ty))),
                    target.span,
                )
            })?;
            let n = self.bcx.ins().iconst(I32, size as i64);
            let desc = self.desc_ptr(ty, target.span)?;
            let fref = self.rt_ref(Rt::RecCopy);
            self.bcx.ins().call(fref, &[dst, from, n, desc]);
            return Ok(());
        }
        let v = self.expr(value)?;
        self.write_place(&place, v, target.span)
    }

    // -------------------------------------------------------------- places

    /// Where a designator lives: a local that holds a *value* is an SSA variable
    /// — the cheaper shape, and what the C back end's registers are — and
    /// everything else is an address: a module variable's data object, a local
    /// record's or array's frame slot, or a heap object reached through a
    /// pointer.
    fn place(&mut self, d: &Designator, span: Span) -> Result<Place, Diagnostic> {
        let name = self.base_name(d)?;
        let mut place = if let Some(var) = self.vars.get(name).copied() {
            let ty = self.local_ty(name, span)?;
            Place::Local(var, self.narrowed(name, ty))
        } else if let Some(addr) = self.slots.get(name).copied() {
            let ty = self.local_ty(name, span)?;
            Place::Addr(addr, self.narrowed(name, ty))
        } else if let Some(gv) = self.gv.get(name).copied() {
            let ty = *self
                .b
                .var_ty
                .get(name)
                .ok_or_else(|| not_yet(&format!("the designator `{name}`"), span))?;
            Place::Addr(
                self.bcx.ins().symbol_value(self.b.ptr, gv),
                self.narrowed(name, ty),
            )
        } else {
            return Err(not_yet(&format!("the designator `{name}`"), span));
        };
        for sel in &d.sels {
            place = self.select(place, sel, d.span)?;
        }
        Ok(place)
    }

    /// The single name a designator starts with. A qualified one (`M.x`) needs
    /// the other module's symbol, which is what importing a *lowered* module
    /// will need (PLAN.md W9.2c).
    fn base_name<'d>(&self, d: &'d Designator) -> Result<&'d str, Diagnostic> {
        match d.base.as_slice() {
            [name] => Ok(name.as_str()),
            _ => Err(not_yet(
                &format!("the qualified designator `{}`", d.base.join(".")),
                d.span,
            )),
        }
    }

    /// The place a bare name denotes, for the statements that work on one
    /// (`FOR`'s control variable).
    fn named_place(&mut self, name: &str, span: Span) -> Result<Place, Diagnostic> {
        let d = Designator {
            base: vec![name.to_string()],
            sels: Vec::new(),
            span,
        };
        self.place(&d, span)
    }

    fn local_ty(&self, name: &str, span: Span) -> Result<TypeId, Diagnostic> {
        self.local_tys
            .get(name)
            .copied()
            .ok_or_else(|| not_yet(&format!("the local `{name}`"), span))
    }

    /// The type a type-`CASE` arm's narrowing gives a name (report §9.5: within
    /// the arm the case variable "is considered as of type T_i"), which is what
    /// makes a field of the *label's* record resolve at the label's offset.
    fn narrowed(&self, name: &str, ty: TypeId) -> TypeId {
        self.narrow
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
            .unwrap_or(ty)
    }

    /// One selector applied to a place: a field's offset (through the base
    /// chain), an index's checked stride, a dereference with its `NIL` check, or
    /// a guard's narrowing.
    fn select(&mut self, place: Place, sel: &Selector, span: Span) -> Result<Place, Diagnostic> {
        match sel {
            Selector::Field(f) => {
                let (base, ty) = self.base_of(&place, span)?;
                let (off, fty) = self
                    .b
                    .field_of(ty, f)
                    .ok_or_else(|| not_yet(&format!("the field `{f}`"), span))?;
                Ok(Place::Addr(
                    self.bcx.ins().iadd_imm_s(base, off as i64),
                    fty,
                ))
            }
            Selector::Index(ix) => {
                let (base, ty) = self.base_of(&place, span)?;
                let (addr, elem) = self.index_addr(base, ty, ix, span)?;
                Ok(Place::Addr(addr, elem))
            }
            Selector::Deref => {
                let (p, ty) = self.pointer_value(&place, span)?;
                self.nil_check(p, span.lo);
                let target = self
                    .b
                    .types
                    .deref(ty)
                    .ok_or_else(|| not_yet("a dereference of a non-pointer", span))?;
                Ok(Place::Addr(p, target))
            }
            Selector::Guard(q) => {
                let t = self
                    .qident_type(q)
                    .ok_or_else(|| not_yet("a guard's type", span))?;
                self.guard_check(&place, t, span)?;
                Ok(match place {
                    Place::Local(v, _) => Place::Local(v, t),
                    Place::Addr(a, _) => Place::Addr(a, t),
                })
            }
            Selector::Call(_) | Selector::Args(_) => {
                Err(not_yet("a call inside a designator", span))
            }
        }
    }

    /// The base a `Field` or an `Index` selector works on: for a pointer the
    /// object it points at — with the `NIL` check the C back end's
    /// `rt_nil(p, at)->f` makes — and otherwise the region's address.
    fn base_of(&mut self, place: &Place, span: Span) -> Result<(ir::Value, TypeId), Diagnostic> {
        match place {
            Place::Local(v, t) => {
                let (value, ty) = (self.bcx.use_var(*v), *t);
                if matches!(self.b.types.get(ty), Type::Pointer { .. }) {
                    self.nil_check(value, span.lo);
                    Ok((value, self.b.types.deref(ty).unwrap_or(TypeId(0))))
                } else {
                    Ok((value, ty))
                }
            }
            Place::Addr(a, t) => {
                if matches!(self.b.types.get(*t), Type::Pointer { .. }) {
                    let p = self
                        .bcx
                        .ins()
                        .load(self.b.ptr, ir::MemFlagsData::new(), *a, 0);
                    self.nil_check(p, span.lo);
                    Ok((p, self.b.types.deref(*t).unwrap_or(TypeId(0))))
                } else {
                    Ok((*a, *t))
                }
            }
        }
    }

    /// The *value* of a place that holds a pointer: a local is already the value
    /// and an address has to be loaded.
    fn pointer_value(
        &mut self,
        place: &Place,
        span: Span,
    ) -> Result<(ir::Value, TypeId), Diagnostic> {
        match place {
            Place::Local(v, t) => Ok((self.bcx.use_var(*v), *t)),
            Place::Addr(a, t) => {
                let ty = self.b.value_type(*t).ok_or_else(|| {
                    not_yet(
                        &format!("a value of type {}", type_name(self.b.types.get(*t))),
                        span,
                    )
                })?;
                Ok((
                    self.bcx.ins().load(ty, ir::MemFlagsData::new(), *a, 0),
                    *t,
                ))
            }
        }
    }

    /// The address of a place, or a refusal when it is a local that holds its
    /// value in a register (which is what `ADR` and a `VAR` actual would need,
    /// PLAN.md W9.2c).
    fn address_of(&mut self, place: &Place, span: Span) -> Result<(ir::Value, TypeId), Diagnostic> {
        match place {
            Place::Addr(a, t) => Ok((*a, *t)),
            Place::Local(..) => Err(not_yet("the address of a local scalar", span)),
        }
    }

    /// Read a place: a local's value, or a load of the place's own type. A region
    /// has no value to read — a copy is `rt_rec_copy`, a *statement* rather than
    /// an expression.
    fn read_place(&mut self, place: &Place, span: Span) -> Result<(ir::Value, TypeId), Diagnostic> {
        match place {
            Place::Local(v, t) => Ok((self.bcx.use_var(*v), *t)),
            Place::Addr(a, t) => {
                let ty = self.b.value_type(*t).ok_or_else(|| {
                    not_yet(
                        &format!(
                            "a value of type {} (a copy is PLAN.md W9.2c)",
                            type_name(self.b.types.get(*t))
                        ),
                        span,
                    )
                })?;
                Ok((
                    self.bcx.ins().load(ty, ir::MemFlagsData::new(), *a, 0),
                    *t,
                ))
            }
        }
    }

    fn write_place(&mut self, place: &Place, v: ir::Value, span: Span) -> Result<(), Diagnostic> {
        match place {
            Place::Local(var, _) => {
                self.bcx.def_var(*var, v);
                Ok(())
            }
            Place::Addr(a, t) => {
                if self.b.value_type(*t).is_none() {
                    return Err(not_yet(
                        &format!("an assignment to {}", type_name(self.b.types.get(*t))),
                        span,
                    ));
                }
                self.bcx.ins().store(ir::MemFlagsData::new(), v, *a, 0);
                Ok(())
            }
        }
    }

    /// The address of one element of an array, and the element's type: row-major,
    /// one `rt_index` per dimension against that dimension's length and the
    /// designator's source byte (which is what makes a failing self-check name
    /// its line, DESIGN §9.4), and the stride from `layout.rs` rather than from a
    /// literal — the element may be a word, a pointer or a record.
    fn index_addr(
        &mut self,
        base: ir::Value,
        ty: TypeId,
        indexes: &[Expr],
        span: Span,
    ) -> Result<(ir::Value, TypeId), Diagnostic> {
        let (dims, elem) = self.b.array_dims(ty).ok_or_else(|| {
            not_yet(
                &format!("an index into {}", type_name(self.b.types.get(ty))),
                span,
            )
        })?;
        if indexes.len() != dims.len() {
            return Err(not_yet(
                &format!("an index with {} subscripts", indexes.len()),
                span,
            ));
        }
        let mut flat: Option<ir::Value> = None;
        for (k, index) in indexes.iter().enumerate() {
            let i = self.expr(index)?;
            let i = if self.b.checks.index {
                let n = self.bcx.ins().iconst(I32, i64::from(dims[k]));
                let at = self.bcx.ins().iconst(I32, i64::from(span.lo));
                self.call_rt(Rt::Index, &[i, n, at])
                    .expect("a bounds check answers with the index")
            } else {
                i
            };
            flat = Some(match flat {
                None => i,
                Some(f) => {
                    let scaled = self.bcx.ins().imul_imm_s(f, i64::from(dims[k]));
                    self.bcx.ins().iadd(scaled, i)
                }
            });
        }
        let flat = flat.expect("an index list has at least one subscript");
        let stride = self.b.layout(elem).map(|l| l.size).unwrap_or(0);
        // Widened before the multiply: an index is an `INTEGER` and the offset is
        // the target's pointer width, as C's own `p[i]` arithmetic is.
        let wide = self.bcx.ins().sextend(self.b.ptr, flat);
        let off = self.bcx.ins().imul_imm_s(wide, stride as i64);
        Ok((self.bcx.ins().iadd(base, off), elem))
    }

    /// The `NIL` check a pointer access carries (DESIGN §9.4): the C back end
    /// makes it inside the access (`rt_nil(p, at)`), so a failure names the
    /// designator it was reading.
    fn nil_check(&mut self, p: ir::Value, at: u32) {
        if !self.b.checks.nil {
            return;
        }
        let ok = self.bcx.create_block();
        let bad = self.bcx.create_block();
        let missing = self.bcx.ins().icmp_imm_s(IntCC::Equal, p, 0);
        self.bcx.ins().brif(missing, bad, &[], ok, &[]);
        self.bcx.switch_to_block(bad);
        let msg = *self
            .msgs
            .get(&Msg::Nil)
            .expect("the module declares every message it can report");
        let text = self.bcx.ins().symbol_value(self.b.ptr, msg);
        let byte = self.offset(at);
        let fref = self.rt_ref(Rt::Trap);
        self.bcx.ins().call(fref, &[text, byte]);
        self.bcx.ins().trap(TrapCode::unwrap_user(1));
        self.bcx.switch_to_block(ok);
    }

    /// `rt_is(descriptor, &TD_T)`: whether an object is of a type, by walking the
    /// descriptor's `base` chain. A `NIL` pointer is not of any record type
    /// (report §6.4), which is answered without reading through it — the C back
    /// end's `IS` would fault there, and its guard spelling already checks.
    fn is_test(&mut self, x: &Expr, target: TypeId, span: Span) -> Result<ir::Value, Diagnostic> {
        let place = match &x.kind {
            ExprKind::Designator(d) => self.place(d, x.span)?,
            _ => {
                return Err(not_yet(
                    "an IS test on something that is not a designator",
                    span,
                ))
            }
        };
        let target_desc = self.desc_ptr(target, span)?;
        let fref = self.rt_ref(Rt::Is);
        let ty = place_ty(&place);
        match self.b.types.get(ty) {
            Type::Pointer { .. } => {
                let (p, _) = self.pointer_value(&place, span)?;
                // Read through the pointer only when it is not NIL: one block
                // answers false, another asks the runtime, and the join carries
                // the answer.
                let some = self.bcx.create_block();
                let none = self.bcx.create_block();
                let done = self.bcx.create_block();
                let res = self.bcx.append_block_param(done, I32);
                let missing = self.bcx.ins().icmp_imm_s(IntCC::Equal, p, 0);
                self.bcx.ins().brif(missing, none, &[], some, &[]);
                self.bcx.switch_to_block(some);
                let d = self
                    .bcx
                    .ins()
                    .load(self.b.ptr, ir::MemFlagsData::new(), p, 0);
                let call = self.bcx.ins().call(fref, &[d, target_desc]);
                let yes = self.bcx.inst_results(call)[0];
                self.bcx.ins().jump(done, &[ir::BlockArg::Value(yes)]);
                self.bcx.switch_to_block(none);
                let no = self.bcx.ins().iconst(I32, 0);
                self.bcx.ins().jump(done, &[ir::BlockArg::Value(no)]);
                self.bcx.switch_to_block(done);
                Ok(res)
            }
            Type::Record { .. } => {
                let (a, _) = self.address_of(&place, span)?;
                let d = self
                    .bcx
                    .ins()
                    .load(self.b.ptr, ir::MemFlagsData::new(), a, 0);
                let call = self.bcx.ins().call(fref, &[d, target_desc]);
                Ok(self.bcx.inst_results(call)[0])
            }
            other => Err(not_yet(
                &format!("an IS test on {}", type_name(other)),
                span,
            )),
        }
    }

    /// A type guard's test, reported by the runtime with the byte *in* the
    /// message the C back end bakes into a literal ("type guard failed at byte
    /// N"): `rt_guard_is`'s shape, with the byte as an argument. A pointer
    /// subject is rejected for `NIL` before its descriptor is read, which is what
    /// the C back end's pointer spelling does.
    fn guard_check(&mut self, place: &Place, target: TypeId, span: Span) -> Result<(), Diagnostic> {
        if !self.b.checks.guard {
            return Ok(());
        }
        let subject = place_ty(place);
        let by_ptr = matches!(self.b.types.get(target), Type::Pointer { .. });
        let held = if by_ptr {
            matches!(self.b.types.get(subject), Type::Pointer { .. })
        } else {
            matches!(self.b.types.get(subject), Type::Record { .. })
        };
        if !held {
            return Err(not_yet(
                &format!(
                    "the guard of {} by {}",
                    type_name(self.b.types.get(subject)),
                    type_name(self.b.types.get(target))
                ),
                span,
            ));
        }
        let target_desc = self.desc_ptr(target, span)?;
        let fref = self.rt_ref(Rt::Is);
        let desc = if by_ptr {
            let (p, _) = self.pointer_value(place, span)?;
            let some = self.bcx.create_block();
            let none = self.bcx.create_block();
            let done = self.bcx.create_block();
            let res = self.bcx.append_block_param(done, I32);
            let missing = self.bcx.ins().icmp_imm_s(IntCC::Equal, p, 0);
            self.bcx.ins().brif(missing, none, &[], some, &[]);
            self.bcx.switch_to_block(some);
            let d = self
                .bcx
                .ins()
                .load(self.b.ptr, ir::MemFlagsData::new(), p, 0);
            let call = self.bcx.ins().call(fref, &[d, target_desc]);
            let yes = self.bcx.inst_results(call)[0];
            self.bcx.ins().jump(done, &[ir::BlockArg::Value(yes)]);
            self.bcx.switch_to_block(none);
            let no = self.bcx.ins().iconst(I32, 0);
            self.bcx.ins().jump(done, &[ir::BlockArg::Value(no)]);
            self.bcx.switch_to_block(done);
            res
        } else {
            let (a, _) = self.address_of(place, span)?;
            let d = self
                .bcx
                .ins()
                .load(self.b.ptr, ir::MemFlagsData::new(), a, 0);
            let call = self.bcx.ins().call(fref, &[d, target_desc]);
            self.bcx.inst_results(call)[0]
        };
        let at = self.offset(span.lo);
        let fref = self.rt_ref(Rt::GuardAt);
        self.bcx.ins().call(fref, &[desc, at]);
        Ok(())
    }

    /// The address of a type's descriptor, which `NEW`, a copy, an `IS` test and
    /// a guard all take. A *pointer* type has none of its own: it names its base
    /// record's, since the descriptor is stored in the object (DESIGN §9.3).
    fn desc_ptr(&mut self, t: TypeId, span: Span) -> Result<ir::Value, Diagnostic> {
        let t = if self.b.types.is_pointer(t) {
            self.b.types.deref(t).unwrap_or(t)
        } else {
            t
        };
        let gv = *self.descs.get(&t).ok_or_else(|| {
            not_yet(
                &format!("a descriptor for {}", type_name(self.b.types.get(t))),
                span,
            )
        })?;
        Ok(self.bcx.ins().symbol_value(self.b.ptr, gv))
    }

    /// Give a region its descriptor, and every record *inside* it its own
    /// (DESIGN §9.3, the C back end's `desc_init`). The descriptor word belongs
    /// to the outermost record — the base-most one, which is at offset 0 of every
    /// view, because an extension embeds its base first — so only this function
    /// writes it, and the base's *fields* are visited for theirs.
    fn desc_init(&mut self, t: TypeId, addr: ir::Value, span: Span) -> Result<(), Diagnostic> {
        match self.b.types.get(t) {
            Type::Record { .. } => {
                let d = self.desc_ptr(t, span)?;
                self.bcx.ins().store(ir::MemFlagsData::new(), d, addr, 0);
                self.desc_init_fields(t, addr, span)
            }
            Type::Array { .. } => {
                let Some((dims, elem)) = self.b.array_dims(t) else {
                    return Ok(());
                };
                if !self.b.contains_record(elem) {
                    return Ok(());
                }
                let stride = self.b.layout(elem).map(|l| l.size).unwrap_or(0);
                self.desc_init_loop(elem, addr, &dims, 0, stride, span)
            }
            _ => Ok(()),
        }
    }

    /// The loop `desc_init` needs for an array: `for i in 0..len { … }`, which is
    /// the C back end's `for (i__ …)`. A nested array recurses into another loop,
    /// so a multi-dimensional array of records is initialised like the C one.
    fn desc_init_loop(
        &mut self,
        elem: TypeId,
        addr: ir::Value,
        dims: &[u32],
        k: usize,
        stride: u64,
        span: Span,
    ) -> Result<(), Diagnostic> {
        if k >= dims.len() {
            return Ok(());
        }
        let head = self.bcx.create_block();
        let body = self.bcx.create_block();
        let after = self.bcx.create_block();
        let i = self.bcx.declare_var(I32);
        let zero = self.bcx.ins().iconst(I32, 0);
        self.bcx.def_var(i, zero);
        self.jump_to(head);
        self.bcx.switch_to_block(head);
        let at = self.bcx.use_var(i);
        let n = self.bcx.ins().iconst(I32, i64::from(dims[k]));
        let more = self.bcx.ins().icmp(IntCC::SignedLessThan, at, n);
        self.bcx.ins().brif(more, body, &[], after, &[]);
        self.bcx.switch_to_block(body);
        let at = self.bcx.use_var(i);
        let wide = self.bcx.ins().sextend(self.b.ptr, at);
        let off = self.bcx.ins().imul_imm_s(wide, stride as i64);
        let here = self.bcx.ins().iadd(addr, off);
        if k + 1 == dims.len() {
            self.desc_init(elem, here, span)?;
        } else {
            self.desc_init_loop(elem, here, dims, k + 1, stride, span)?;
        }
        let at = self.bcx.use_var(i);
        let next = self.bcx.ins().iadd_imm_s(at, 1);
        self.bcx.def_var(i, next);
        self.jump_to(head);
        self.bcx.switch_to_block(after);
        Ok(())
    }

    /// Visit the record-valued fields of a record and of its bases, each at its
    /// own offset (the C back end's `desc_init_fields`).
    fn desc_init_fields(
        &mut self,
        t: TypeId,
        addr: ir::Value,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let Some(r) = self.b.records.get(&t).cloned() else {
            return Ok(());
        };
        if let Some(b) = r.base {
            // The base is the first member, so its fields are at this address.
            self.desc_init_fields(b, addr, span)?;
        }
        for (_, off, ty) in &r.fields {
            if self.b.contains_record(*ty) {
                let here = self.bcx.ins().iadd_imm_s(addr, *off as i64);
                self.desc_init(*ty, here, span)?;
            }
        }
        Ok(())
    }

    /// `NEW(p)`: the allocation goes through the runtime, so the object's own
    /// descriptor is stored at its head — which is what a guard and a type-`CASE`
    /// read — and the report's failure (§6.4: the pointer becomes `NIL`) is the
    /// runtime's single answer. The descriptors of the records *inside* the new
    /// object are written here, as the C back end's `NEW` does.
    fn new_stmt(&mut self, args: &[Expr], span: Span) -> Result<(), Diagnostic> {
        let arg = args
            .first()
            .ok_or_else(|| not_yet("NEW without a pointer", span))?;
        let place = match &arg.kind {
            ExprKind::Designator(d) => self.place(d, arg.span)?,
            _ => return Err(not_yet("NEW of something that is not a designator", span)),
        };
        let ty = place_ty(&place);
        let Some(rec) = self.b.types.deref(ty) else {
            return Err(not_yet(
                &format!("NEW of {}", type_name(self.b.types.get(ty))),
                span,
            ));
        };
        if !matches!(self.b.types.get(rec), Type::Record { .. }) {
            return Err(not_yet(
                &format!("NEW of a pointer to {}", type_name(self.b.types.get(rec))),
                span,
            ));
        }
        let size = self
            .b
            .layout(rec)
            .map(|l| l.size)
            .ok_or_else(|| not_yet("NEW of a record without a layout", span))?;
        let desc = self.desc_ptr(rec, span)?;
        // The allocation is kept in one value and stored once, so this is the
        // shape R26 asks of the C path: the pointer is written and then read for
        // the descriptor walk, where C re-evaluates it.
        let size_v = self.bcx.ins().iconst(self.b.ptr, size as i64);
        let fref = self.rt_ref(Rt::New);
        let call = self.bcx.ins().call(fref, &[size_v, desc]);
        let object = self.bcx.inst_results(call)[0];
        self.write_place(&place, object, span)?;
        let nonnull = self.bcx.ins().icmp_imm_s(IntCC::NotEqual, object, 0);
        let ok = self.bcx.create_block();
        let done = self.bcx.create_block();
        self.bcx.ins().brif(nonnull, ok, &[], done, &[]);
        self.bcx.switch_to_block(ok);
        self.desc_init(rec, object, span)?;
        self.jump_to(done);
        self.bcx.switch_to_block(done);
        Ok(())
    }

    /// The module body's preamble: every variable that can hold a pointer is
    /// registered with the collector, and every record variable is given its
    /// descriptor — the C back end's `__init` before its first statement
    /// (DESIGN §15.4).
    fn init_preamble(&mut self, span: Span) -> Result<(), Diagnostic> {
        let roots: Vec<(String, ir::GlobalValue, i32)> = self
            .roots
            .iter()
            .map(|(n, (gv, count))| (n.clone(), *gv, *count))
            .collect();
        for (name, map, n) in roots {
            let base = match self.gv.get(&name).copied() {
                Some(gv) => self.bcx.ins().symbol_value(self.b.ptr, gv),
                None => continue,
            };
            let fields = self.bcx.ins().symbol_value(self.b.ptr, map);
            let count = self.bcx.ins().iconst(I32, i64::from(n));
            let fref = self.rt_ref(Rt::GcRoot);
            self.bcx.ins().call(fref, &[base, fields, count]);
        }
        let records: Vec<(String, TypeId)> = self
            .b
            .var_ty
            .iter()
            .filter(|(_, t)| self.b.contains_record(**t))
            .map(|(n, t)| (n.clone(), *t))
            .collect();
        for (name, ty) in records {
            let Some(gv) = self.gv.get(&name).copied() else {
                continue;
            };
            let addr = self.bcx.ins().symbol_value(self.b.ptr, gv);
            self.desc_init(ty, addr, span)?;
        }
        Ok(())
    }

    fn rt_ref(&self, rt: Rt) -> ir::FuncRef {
        *self
            .rt
            .get(&rt)
            .expect("the runtime entry points are declared for the module")
    }

    /// `IF … ELSIF … ELSE … END`, as a chain of blocks rather than nested
    /// conditions, so that each arm's setup sits where the arm is reached.
    fn if_stmt(
        &mut self,
        arms: &[(Expr, Vec<Stmt>)],
        els: Option<&[Stmt]>,
    ) -> Result<(), Diagnostic> {
        let after = self.bcx.create_block();
        for (cond, body) in arms {
            let yes = self.bcx.create_block();
            let no = self.bcx.create_block();
            self.branch(cond, yes, no)?;
            self.bcx.switch_to_block(yes);
            self.stmts(body)?;
            self.jump_to(after);
            self.bcx.switch_to_block(no);
        }
        if let Some(body) = els {
            self.stmts(body)?;
        }
        self.jump_to(after);
        self.bcx.switch_to_block(after);
        Ok(())
    }

    /// report §9.6: the guards are tried in turn until one holds, and when none
    /// does the loop ends. So `ELSIF` here is a sequence of *guards*, not a
    /// short-circuit `OR`.
    fn while_stmt(&mut self, arms: &[(Expr, Vec<Stmt>)]) -> Result<(), Diagnostic> {
        let head = self.bcx.create_block();
        let after = self.bcx.create_block();
        self.jump_to(head);
        for (cond, body) in arms {
            self.bcx.switch_to_block(head);
            let yes = self.bcx.create_block();
            let no = self.bcx.create_block();
            self.branch(cond, yes, no)?;
            self.bcx.switch_to_block(yes);
            self.loops.push(after);
            self.stmts(body)?;
            self.loops.pop();
            self.jump_to(head);
            self.bcx.switch_to_block(no);
        }
        self.jump_to(after);
        self.bcx.switch_to_block(after);
        Ok(())
    }

    fn repeat_stmt(&mut self, body: &[Stmt], until: &Expr) -> Result<(), Diagnostic> {
        let head = self.bcx.create_block();
        let after = self.bcx.create_block();
        self.jump_to(head);
        self.bcx.switch_to_block(head);
        self.loops.push(after);
        self.stmts(body)?;
        self.loops.pop();
        // The guard is evaluated where the body leaves it, which is what
        // `REPEAT … UNTIL` means: the body runs at least once.
        if !self.ended() {
            let yes = self.bcx.create_block();
            self.branch(until, yes, head)?;
            self.bcx.switch_to_block(yes);
        }
        self.jump_to(after);
        self.bcx.switch_to_block(after);
        Ok(())
    }

    /// `FOR v := b TO e BY k DO S END`, and report §9.8's equivalence: the bound
    /// is evaluated again on each iteration, as the desugaring says and as the C
    /// back end's `for` header does. `BY` takes a *constant* expression (§9.8),
    /// so it is folded here and the comparison is chosen once.
    fn for_stmt(
        &mut self,
        var: &str,
        from: &Expr,
        to: &Expr,
        by: Option<&Expr>,
        body: &[Stmt],
        span: Span,
    ) -> Result<(), Diagnostic> {
        let step = match by {
            Some(k) => match const_eval::eval_with(k, &|x| self.b.sema.const_at(x.span)) {
                Ok(const_eval::Const::Int(v)) => v,
                _ => {
                    return Err(Diagnostic::error(
                        k.span,
                        "the clif back end needs BY's increment as an INTEGER constant (report 9.8)",
                    ))
                }
            },
            None => 1,
        };
        // Check 34 rejects `BY 0`, so the direction is decidable here.
        let up = step > 0;
        let start = self.expr(from)?;
        let control = self.named_place(var, span)?;
        self.write_place(&control, start, span)?;
        let head = self.bcx.create_block();
        let body_b = self.bcx.create_block();
        let after = self.bcx.create_block();
        self.jump_to(head);
        self.bcx.switch_to_block(head);
        let (v, _) = self.read_place(&control, span)?;
        let bound = self.expr(to)?;
        let test = if up {
            self.bcx.ins().icmp(IntCC::SignedLessThanOrEqual, v, bound)
        } else {
            self.bcx.ins().icmp(IntCC::SignedGreaterThanOrEqual, v, bound)
        };
        self.bcx.ins().brif(test, body_b, &[], after, &[]);
        self.bcx.switch_to_block(body_b);
        self.loops.push(after);
        self.stmts(body)?;
        self.loops.pop();
        if !self.ended() {
            let (v, _) = self.read_place(&control, span)?;
            let next = self.bcx.ins().iadd_imm_s(v, i64::from(step));
            self.write_place(&control, next, span)?;
            self.jump_to(head);
        }
        self.bcx.switch_to_block(after);
        Ok(())
    }

    /// A `CASE`: for a scalar subject a chain of comparisons, and for a
    /// record/pointer subject a chain on the descriptor's `id` (DESIGN §9.3),
    /// which is the C back end's `switch (RT_DESC(v)->id)`. A subject no arm
    /// matches traps — W2.5's decision for the scalar form, and the type form's
    /// trapping default — and an arm's body sees the case variable narrowed to
    /// the label's type (report §9.5).
    fn case_stmt(
        &mut self,
        subject: &Expr,
        arms: &[CaseArm],
        span: Span,
    ) -> Result<(), Diagnostic> {
        let ty = self.type_of(subject, span)?;
        if matches!(
            self.b.types.get(ty),
            Type::Pointer { .. } | Type::Record { .. }
        ) {
            return self.type_case(subject, arms, span);
        }
        match self.b.types.get(ty) {
            Type::Basic(Basic::Integer) | Type::Basic(Basic::Boolean) => {}
            other => return Err(not_yet(&format!("a CASE over {}", type_name(other)), span)),
        }
        let value = self.expr(subject)?;
        let after = self.bcx.create_block();
        let mut pending = self.bcx.create_block();
        self.jump_to(pending);
        let mut default: Option<&Vec<Stmt>> = None;
        for arm in arms {
            if arm.labels.as_ref().is_none_or(|l| l.is_empty()) {
                default = Some(&arm.body);
                continue;
            }
            self.bcx.switch_to_block(pending);
            let test = self.label_test(value, arm)?;
            let body_b = self.bcx.create_block();
            let next = self.bcx.create_block();
            self.bcx.ins().brif(test, body_b, &[], next, &[]);
            self.bcx.switch_to_block(body_b);
            self.stmts(&arm.body)?;
            self.jump_to(after);
            pending = next;
        }
        self.bcx.switch_to_block(pending);
        match default {
            Some(body) => {
                self.stmts(body)?;
                self.jump_to(after);
            }
            // No default arm: a value that matches no label traps, as it does in
            // the type-`CASE` (§9.3) — W2.5's decision, and what the C back end
            // emits (`default: rt_trap_pos("CASE: no matching label")`). The
            // report leaves the case undefined (§9.5 defines which arm runs and
            // says nothing about a value that matches no label), and this
            // design's answer to undefined is a located trap. `-Ochecks=case`
            // still governs it; with the check off, the run falls out of the arms
            // as it always did.
            None if self.b.checks.case_ => self.trap(Msg::CaseDefault, subject.span.lo),
            None => self.jump_to(after),
        }
        self.bcx.switch_to_block(after);
        Ok(())
    }

    /// Whether a scalar subject is one of an arm's labels: an equality for a
    /// single label, an interval for a range, and a disjunction of those for the
    /// labels that share the arm.
    fn label_test(&mut self, value: ir::Value, arm: &CaseArm) -> Result<ir::Value, Diagnostic> {
        let mut test: Option<ir::Value> = None;
        for l in arm.labels.as_deref().unwrap_or(&[]) {
            let lo = case_label(&l.lo, arm.span)?;
            let one = match &l.hi {
                None => self.bcx.ins().icmp_imm_s(IntCC::Equal, value, i64::from(lo)),
                Some(h) => {
                    let hi = case_label(h, arm.span)?;
                    if hi == lo {
                        self.bcx.ins().icmp_imm_s(IntCC::Equal, value, i64::from(lo))
                    } else {
                        let a = self.bcx.ins().icmp_imm_s(
                            IntCC::SignedGreaterThanOrEqual,
                            value,
                            i64::from(lo),
                        );
                        let b = self
                            .bcx
                            .ins()
                            .icmp_imm_s(IntCC::SignedLessThanOrEqual, value, i64::from(hi));
                        self.bcx.ins().band(a, b)
                    }
                }
            };
            test = Some(match test {
                None => one,
                Some(t) => self.bcx.ins().bor(t, one),
            });
        }
        Ok(test.expect("an arm with labels has at least one"))
    }

    /// A type-`CASE`: the subject must be a designator (the arm narrows its
    /// *name*), the test is the descriptor's `id`, and a subject no label matches
    /// traps — with the C back end's message and byte. A `NIL` pointer matches no
    /// label, which the C back end instead faults on.
    fn type_case(
        &mut self,
        subject: &Expr,
        arms: &[CaseArm],
        span: Span,
    ) -> Result<(), Diagnostic> {
        let (place, name) = match &subject.kind {
            ExprKind::Designator(d) => (self.place(d, subject.span)?, d.base[0].clone()),
            _ => {
                return Err(not_yet(
                    "a type-CASE over something that is not a designator",
                    span,
                ))
            }
        };
        let ty = place_ty(&place);
        let id_off = crate::layout::ob_desc(self.b.ptr_bytes).id;
        let desc = match self.b.types.get(ty) {
            Type::Pointer { .. } => {
                let (p, _) = self.pointer_value(&place, subject.span)?;
                // A NIL pointer has no descriptor to read: it matches no label,
                // so the chain is entered with an id no label can have. That
                // keeps the report's "no label matches" an answer rather than a
                // fault.
                let some = self.bcx.create_block();
                let none = self.bcx.create_block();
                let done = self.bcx.create_block();
                let res = self.bcx.append_block_param(done, I32);
                let missing = self.bcx.ins().icmp_imm_s(IntCC::Equal, p, 0);
                self.bcx.ins().brif(missing, none, &[], some, &[]);
                self.bcx.switch_to_block(some);
                let d = self
                    .bcx
                    .ins()
                    .load(self.b.ptr, ir::MemFlagsData::new(), p, 0);
                let yes = self
                    .bcx
                    .ins()
                    .load(I32, ir::MemFlagsData::new(), d, id_off as i32);
                self.bcx.ins().jump(done, &[ir::BlockArg::Value(yes)]);
                self.bcx.switch_to_block(none);
                let no = self.bcx.ins().iconst(I32, -1);
                self.bcx.ins().jump(done, &[ir::BlockArg::Value(no)]);
                self.bcx.switch_to_block(done);
                res
            }
            Type::Record { .. } => {
                let (a, _) = self.address_of(&place, subject.span)?;
                let d = self
                    .bcx
                    .ins()
                    .load(self.b.ptr, ir::MemFlagsData::new(), a, 0);
                self.bcx
                    .ins()
                    .load(I32, ir::MemFlagsData::new(), d, id_off as i32)
            }
            other => {
                return Err(not_yet(
                    &format!("a type-CASE over {}", type_name(other)),
                    span,
                ))
            }
        };
        let after = self.bcx.create_block();
        let mut pending = self.bcx.create_block();
        self.jump_to(pending);
        let mut default: Option<&Vec<Stmt>> = None;
        for arm in arms {
            let labels: Vec<TypeId> = arm
                .labels
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .filter_map(|l| match &l.lo {
                    CaseLabel::Named(q) => self.qident_type(q),
                    _ => None,
                })
                .collect();
            if labels.is_empty() {
                default = Some(&arm.body);
                continue;
            }
            self.bcx.switch_to_block(pending);
            let mut test: Option<ir::Value> = None;
            for t in &labels {
                let rec = if self.b.types.is_pointer(*t) {
                    self.b.types.deref(*t).unwrap_or(*t)
                } else {
                    *t
                };
                // A label's record is what the test compares ids with, so a type
                // the module has no descriptor for cannot be one.
                if self.b.descriptor(rec).is_none() {
                    return Err(not_yet(
                        &format!("a label naming {}", type_name(self.b.types.get(rec))),
                        span,
                    ));
                }
                let id = self.bcx.ins().iconst(I32, i64::from(rec.0));
                let one = self.bcx.ins().icmp(IntCC::Equal, desc, id);
                test = Some(match test {
                    None => one,
                    Some(t) => self.bcx.ins().bor(t, one),
                });
            }
            let test = test.expect("an arm with labels has at least one");
            let body_b = self.bcx.create_block();
            let next = self.bcx.create_block();
            self.bcx.ins().brif(test, body_b, &[], next, &[]);
            self.bcx.switch_to_block(body_b);
            let mut scope = HashMap::new();
            scope.insert(name.clone(), labels[0]);
            self.narrow.push(scope);
            self.stmts(&arm.body)?;
            self.narrow.pop();
            self.jump_to(after);
            pending = next;
        }
        self.bcx.switch_to_block(pending);
        match default {
            Some(body) => {
                self.stmts(body)?;
                self.jump_to(after);
            }
            None if self.b.checks.case_ => self.trap(Msg::CaseType, subject.span.lo),
            None => self.jump_to(after),
        }
        self.bcx.switch_to_block(after);
        Ok(())
    }

    /// A call used as a statement: a procedure of this module, or one of the
    /// predeclared proper procedures this slice lowers — `ASSERT` and `NEW`.
    fn call_stmt(&mut self, d: &Designator) -> Result<(), Diagnostic> {
        let name = self.base_name(d)?;
        let args = self.call_args(d)?.to_vec();
        if let Some(fref) = self.frefs.get(name).copied() {
            let vals = self.arg_values(&args)?;
            self.bcx.ins().call(fref, &vals);
            return Ok(());
        }
        if !self.vars.contains_key(name) && !self.shadows(name) {
            match name {
                "ASSERT" => {
                    let cond = args
                        .first()
                        .ok_or_else(|| not_yet("an ASSERT without a condition", d.span))?;
                    let ok = self.bcx.create_block();
                    let bad = self.bcx.create_block();
                    self.branch(cond, ok, bad)?;
                    self.bcx.switch_to_block(bad);
                    self.trap(Msg::Assertion, d.span.lo);
                    self.bcx.switch_to_block(ok);
                    return Ok(());
                }
                "NEW" => return self.new_stmt(&args, d.span),
                _ => {}
            }
        }
        Err(not_yet(&format!("the call to `{name}`"), d.span))
    }

    /// A call's formal actual parameters: the resolver turned the parenthesised
    /// group into `Selector::Call` (DESIGN §5 D2).
    fn call_args<'d>(&self, d: &'d Designator) -> Result<&'d [Expr], Diagnostic> {
        match d.sels.as_slice() {
            [Selector::Call(args)] => Ok(args),
            _ => Err(not_yet(&format!("the call `{}`", d.base.join(".")), d.span)),
        }
    }

    fn arg_values(&mut self, args: &[Expr]) -> Result<Vec<ir::Value>, Diagnostic> {
        let mut vals = Vec::with_capacity(args.len());
        for a in args {
            vals.push(self.expr(a)?);
        }
        Ok(vals)
    }

    /// Whether a name is a module-level object of this module (a variable or a
    /// procedure), which is what stops a predeclared name from being taken for
    /// the built-in.
    fn shadows(&self, name: &str) -> bool {
        self.b.vars.contains_key(name) || self.b.procs.contains_key(name)
    }

    /// A call's *value*: `MIN`/`MAX` are constants the checker recorded, the
    /// predeclared functions the fragment uses are IR (`ABS`, `ODD` and the three
    /// shifts), and a procedure of this module is a call.
    fn call_value(&mut self, d: &Designator, span: Span) -> Result<ir::Value, Diagnostic> {
        // A user's own `MIN` records nothing, so shadowing needs no check: the C
        // back end reads the same table the same way.
        if let Some(v) = self.b.sema.extreme_at(span) {
            return Ok(self.bcx.ins().iconst(I32, i64::from(v)));
        }
        let name = self.base_name(d)?.to_string();
        if let Some(fref) = self.frefs.get(&name).copied() {
            let args = self.call_args(d)?.to_vec();
            let vals = self.arg_values(&args)?;
            let call = self.bcx.ins().call(fref, &vals);
            return self
                .bcx
                .inst_results(call)
                .first()
                .copied()
                .ok_or_else(|| not_yet(&format!("the call to `{name}` as a value"), span));
        }
        // A name that is a variable or a module object here is a procedure value,
        // so this is a call through a pointer, which needs a signature in the IR
        // (PLAN.md W9.2c).
        if self.vars.contains_key(&name) || self.shadows(&name) {
            return Err(not_yet(
                &format!("the call through the procedure variable `{name}`"),
                span,
            ));
        }
        let args = self.call_args(d)?.to_vec();
        match name.as_str() {
            // `ABS` reads its operand once, so the IR's own conditional is
            // enough; the C path needs a helper for exactly the same reason.
            "ABS" => {
                let x = self.expr(&args[0])?;
                let neg = self.bcx.ins().ineg(x);
                let below = self.bcx.ins().icmp_imm_s(IntCC::SignedLessThan, x, 0);
                Ok(self.bcx.ins().select(below, neg, x))
            }
            "ODD" => {
                let x = self.expr(&args[0])?;
                Ok(self.bcx.ins().band_imm_s(x, 1))
            }
            "LSL" | "ASR" | "ROR" => {
                let x = self.expr(&args[0])?;
                let n = self.expr(&args[1])?;
                self.shift(&name, x, n)
            }
            _ => Err(not_yet(&format!("the predeclared function `{name}`"), span)),
        }
    }

    /// The report's shifts (report §8.2.2) — the one rule the IR does not share
    /// (DESIGN §16). `LSL(x,n)` is `x * 2^n` and `ASR(x,n)` is `x DIV 2^n`, so a
    /// count of 32 or more is *defined* — 0, and for `ASR` 0 or -1 — while
    /// Cranelift's own shifts keep only the low five bits of the count, which is
    /// what R22 had to fix in the C runtime. The count is therefore masked here,
    /// and the wide case is answered from the sign: the corpus generates counts
    /// of 32, 33 and 63 on purpose, and `LSL(x, 33)` has to be 0.
    fn shift(&mut self, name: &str, x: ir::Value, n: ir::Value) -> Result<ir::Value, Diagnostic> {
        let k = self.bcx.ins().band_imm_s(n, 31);
        match name {
            // A rotation is the report's *definition* mod 32, so two shifts by
            // complementary amounts are it: `ROR` alone masks.
            "ROR" => {
                let lo = self.bcx.ins().ushr(x, k);
                let neg = self.bcx.ins().ineg(k);
                let k2 = self.bcx.ins().band_imm_s(neg, 31);
                let hi = self.bcx.ins().ishl(x, k2);
                Ok(self.bcx.ins().bor(lo, hi))
            }
            "LSL" | "ASR" => {
                let inside = if name == "LSL" {
                    self.bcx.ins().ishl(x, k)
                } else {
                    self.bcx.ins().sshr(x, k)
                };
                let wide = if name == "LSL" {
                    self.bcx.ins().iconst(I32, 0)
                } else {
                    // `x DIV 2^32` is 0 for a positive x and -1 for a negative
                    // one: x's sign, extended over the whole word.
                    self.bcx.ins().sshr_imm_s(x, 31)
                };
                let far = self
                    .bcx
                    .ins()
                    .icmp_imm_s(IntCC::SignedGreaterThanOrEqual, n, 32);
                Ok(self.bcx.ins().select(far, wide, inside))
            }
            _ => Err(host(format!("no shift called `{name}`"))),
        }
    }

    // ----------------------------------------------------------- expressions

    /// A boolean expression as a value, which is what C's `&&`/`||` answer and
    /// what a `BOOLEAN` is here: an `int32_t` holding 0 or 1 (DESIGN §9.2).
    fn bool_value(&mut self, e: &Expr) -> Result<ir::Value, Diagnostic> {
        match &e.kind {
            // `&` and `OR` evaluate their right operand only when the left one
            // does not decide the answer: C's `&&`/`||` do that, `e2e` pins the
            // difference in both directions, and an IR *value* cannot — so it is
            // two blocks and a merge, with the answer as the join's argument.
            ExprKind::Binary(BinOp::And | BinOp::Or, ..) => {
                let yes = self.bcx.create_block();
                let no = self.bcx.create_block();
                let done = self.bcx.create_block();
                let value = self.bcx.append_block_param(done, I32);
                self.branch(e, yes, no)?;
                self.bcx.switch_to_block(yes);
                let one = self.bcx.ins().iconst(I32, 1);
                self.bcx.ins().jump(done, &[ir::BlockArg::Value(one)]);
                self.bcx.switch_to_block(no);
                let zero = self.bcx.ins().iconst(I32, 0);
                self.bcx.ins().jump(done, &[ir::BlockArg::Value(zero)]);
                self.bcx.switch_to_block(done);
                Ok(value)
            }
            _ => {
                let t = self.truth(e)?;
                Ok(self.bcx.ins().uextend(I32, t))
            }
        }
    }

    /// A condition as the `i8` a branch takes. `&`/`OR`/`~` are answered from
    /// their operands' truth values, which is what makes the short-circuiting
    /// fall out of `branch`.
    fn truth(&mut self, e: &Expr) -> Result<ir::Value, Diagnostic> {
        match &e.kind {
            ExprKind::Binary(op, l, r) if is_relation(*op) => {
                let a = self.expr(l)?;
                let b = self.expr(r)?;
                // A pointer comparison is an address comparison, and the two
                // operands are the same IR type because the checker made them the
                // same Oberon type (an `INTEGER` against `NIL` never reaches here:
                // `NIL` is only comparable with a pointer).
                Ok(self.bcx.ins().icmp(relation_cc(*op), a, b))
            }
            ExprKind::Unary(UnOp::Not, x) => {
                let t = self.truth(x)?;
                Ok(self.bcx.ins().bxor_imm_s(t, 1))
            }
            ExprKind::Bool(b) => Ok(self.bcx.ins().iconst(types::I8, i64::from(*b))),
            _ => {
                let v = self.expr(e)?;
                Ok(self.bcx.ins().icmp_imm_s(IntCC::NotEqual, v, 0))
            }
        }
    }

    /// Branch on a condition. `&` and `OR` become control flow here, and that is
    /// where their right operand *stops* being evaluated when the left one
    /// decides the answer.
    fn branch(&mut self, e: &Expr, yes: ir::Block, no: ir::Block) -> Result<(), Diagnostic> {
        match &e.kind {
            ExprKind::Binary(BinOp::And, l, r) => {
                let mid = self.bcx.create_block();
                self.branch(l, mid, no)?;
                self.bcx.switch_to_block(mid);
                self.branch(r, yes, no)
            }
            ExprKind::Binary(BinOp::Or, l, r) => {
                let mid = self.bcx.create_block();
                self.branch(l, yes, mid)?;
                self.bcx.switch_to_block(mid);
                self.branch(r, yes, no)
            }
            ExprKind::Unary(UnOp::Not, x) => self.branch(x, no, yes),
            ExprKind::Bool(b) => {
                self.bcx.ins().jump(if *b { yes } else { no }, &[]);
                Ok(())
            }
            _ => {
                let t = self.truth(e)?;
                self.bcx.ins().brif(t, yes, &[], no, &[]);
                Ok(())
            }
        }
    }

    fn expr(&mut self, e: &Expr) -> Result<ir::Value, Diagnostic> {
        match &e.kind {
            // `INTEGER`'s bits: `2147483648` is `MIN(INTEGER)`, which is why the
            // literal arrives as a `u32` (DESIGN §4).
            ExprKind::Int(v) => Ok(self.bcx.ins().iconst(I32, i64::from(*v as i32))),
            ExprKind::Bool(b) => Ok(self.bcx.ins().iconst(I32, i64::from(*b))),
            ExprKind::Nil => Ok(self.bcx.ins().iconst(self.b.ptr, 0)),
            ExprKind::Designator(d) => self.designator_value(d, e.span),
            ExprKind::TypeTest(x, q) => {
                let target = self
                    .qident_type(q)
                    .ok_or_else(|| not_yet("the type of an IS test", e.span))?;
                self.is_test(x, target, e.span)
            }
            ExprKind::Unary(op, x) => {
                let v = self.expr(x)?;
                Ok(match op {
                    UnOp::Plus => v,
                    UnOp::Neg => self.bcx.ins().ineg(v),
                    // `~` is the boolean negation (report §8.2.1) and a BOOLEAN
                    // is 0 or 1, so one bit does it.
                    UnOp::Not => self.bcx.ins().bxor_imm_s(v, 1),
                })
            }
            ExprKind::Binary(op, l, r) => self.binary_value(*op, l, r, e),
            other => Err(not_yet(expression_name(other), e.span)),
        }
    }

    fn binary_value(
        &mut self,
        op: BinOp,
        l: &Expr,
        r: &Expr,
        whole: &Expr,
    ) -> Result<ir::Value, Diagnostic> {
        // A comparison, and `&`/`OR`, are conditions: as a *value* they are a
        // BOOLEAN, which is what `bool_value` answers with — short-circuiting
        // included.
        if is_relation(op) || matches!(op, BinOp::And | BinOp::Or) {
            return self.bool_value(whole);
        }
        let a = self.expr(l)?;
        let b = self.expr(r)?;
        Ok(match op {
            BinOp::Add => self.bcx.ins().iadd(a, b),
            BinOp::Sub => self.bcx.ins().isub(a, b),
            // Wrapping is the IR's own rule for `iadd`/`isub`/`imul`, which is
            // exactly what `-fwrapv` buys the C path (DESIGN §9.1, R4).
            BinOp::Mul => self.bcx.ins().imul(a, b),
            // `DIV`, and integer `/`, are floor division with the divisor check
            // (report §8.2.2; the folder defines `/` as `DIV`, so compile time
            // and run time have to agree). Both are the runtime's rule and the
            // runtime's message, and the byte is the left operand's, as in the
            // emitted C.
            BinOp::IDiv | BinOp::Div => {
                let at = self.offset(l.span.lo);
                self.call_rt(Rt::Div, &[a, b, at])
                    .expect("a divisor check answers with the quotient")
            }
            BinOp::Mod => {
                let at = self.offset(l.span.lo);
                self.call_rt(Rt::Mod, &[a, b, at])
                    .expect("a divisor check answers with the remainder")
            }
            other => return Err(not_yet(&format!("the operator {other:?}"), whole.span)),
        })
    }

    fn designator_value(&mut self, d: &Designator, span: Span) -> Result<ir::Value, Diagnostic> {
        if matches!(d.sels.last(), Some(Selector::Call(_))) {
            return self.call_value(d, span);
        }
        // A designator that names a constant *is* its value: the checker recorded
        // it where it is used, so the folding the C path needs for C's warnings
        // has already happened (R20, PLAN.md W6.2).
        if let Some(c) = self.b.sema.const_at(span) {
            return match c.int() {
                Some(v) => Ok(self.bcx.ins().iconst(I32, i64::from(v))),
                None => Err(not_yet("a non-INTEGER constant", span)),
            };
        }
        let place = self.place(d, span)?;
        let (v, _) = self.read_place(&place, span)?;
        Ok(v)
    }

    /// The type of an expression, as the checker recorded it.
    fn type_of(&self, e: &Expr, span: Span) -> Result<TypeId, Diagnostic> {
        self.b
            .sema
            .type_at(e.span, self.b.types)
            .ok_or_else(|| not_yet("an expression without a type", span))
    }

    /// The type a guard's or a type-`CASE` label's name denotes: a declaration of
    /// this module (a label cannot name another module's type — report §9.5's
    /// labels are the module's own).
    fn qident_type(&self, q: &crate::ast::QIdent) -> Option<TypeId> {
        if q.len() != 1 {
            return None;
        }
        let decl = self
            .b
            .module
            .decls
            .types
            .iter()
            .find(|d| d.name == q[0])?;
        self.b
            .sema
            .type_at(span_of_type(&decl.ty), self.b.types)
    }

    // --------------------------------------------------------------- helpers

    fn offset(&mut self, at: u32) -> ir::Value {
        self.bcx.ins().iconst(I32, i64::from(at))
    }

    fn call_rt(&mut self, rt: Rt, args: &[ir::Value]) -> Option<ir::Value> {
        let fref = *self
            .rt
            .get(&rt)
            .expect("the runtime entry points are declared for the module");
        let call = self.bcx.ins().call(fref, args);
        self.bcx.inst_results(call).first().copied()
    }

    /// A trap the back end reports itself: the runtime reports, and the path
    /// ends. A signature has no `noreturn` bit (DESIGN §16), so the successor is
    /// unreachable *because the emitter says so* — the trap is the IR's own, and
    /// nothing falls through into what the surrounding construct put after it.
    fn trap(&mut self, msg: Msg, at: u32) {
        let fref = *self
            .rt
            .get(&Rt::Trap)
            .expect("the runtime entry points are declared for the module");
        let text = *self
            .msgs
            .get(&msg)
            .expect("the module declares every message it can report");
        let m = self.bcx.ins().symbol_value(self.b.ptr, text);
        let a = self.offset(at);
        self.bcx.ins().call(fref, &[m, a]);
        self.bcx.ins().trap(TrapCode::unwrap_user(1));
    }

    /// Whether the block being built already ends in a terminator, which is when
    /// another instruction would have nowhere to go. Cranelift's own
    /// `is_unreachable` answers a *different* question — a sealed block with no
    /// predecessors — so the emitter asks this one, which is what a `RETURN`
    /// mid-arm leaves behind.
    fn ended(&self) -> bool {
        self.bcx.current_block().is_some_and(|b| {
            self.bcx
                .func
                .layout
                .last_inst(b)
                .is_some_and(|i| self.bcx.func.dfg.insts[i].opcode().is_terminator())
        })
    }

    /// Finish the block being built with a jump, unless control has left it
    /// already. Every block the emitter opens ends in a terminator, which is what
    /// the IR requires and what keeps a `RETURN` inside an arm from leaving a
    /// block the builder still has to fill.
    fn jump_to(&mut self, target: ir::Block) {
        if !self.ended() {
            self.bcx.ins().jump(target, &[]);
        }
    }
}

fn is_relation(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
    )
}

/// The fragment's relations are over `INTEGER`, which is signed (DESIGN §9.2),
/// and over `BOOLEAN`, where equality is equality either way.
fn relation_cc(op: BinOp) -> IntCC {
    match op {
        BinOp::Eq => IntCC::Equal,
        BinOp::Ne => IntCC::NotEqual,
        BinOp::Lt => IntCC::SignedLessThan,
        BinOp::Le => IntCC::SignedLessThanOrEqual,
        BinOp::Gt => IntCC::SignedGreaterThan,
        _ => IntCC::SignedGreaterThanOrEqual,
    }
}

/// An integer `CASE` label. The report's other three kinds are a `CHAR`, a
/// string and a type name, and each needs a type the fragment does not lower.
fn case_label(l: &CaseLabel, span: Span) -> Result<i32, Diagnostic> {
    match l {
        CaseLabel::Int(v) => Ok(*v as i32),
        CaseLabel::Char(c) => Ok(i32::from(*c)),
        CaseLabel::Str(_) | CaseLabel::Named(_) => Err(not_yet("a non-integer CASE label", span)),
    }
}

/// The name a refused expression reads with.
fn expression_name(e: &ExprKind) -> &'static str {
    match e {
        ExprKind::Real(_) => "a REAL literal",
        ExprKind::Char(_) => "a CHAR literal",
        ExprKind::Str(_) => "a string",
        ExprKind::Nil => "NIL",
        ExprKind::Set(_) => "a SET",
        ExprKind::TypeTest(..) => "an IS test",
        ExprKind::Int(_)
        | ExprKind::Bool(_)
        | ExprKind::Designator(_)
        | ExprKind::Unary(..)
        | ExprKind::Binary(..) => "this expression",
    }
}
