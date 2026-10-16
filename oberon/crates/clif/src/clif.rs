//! The Cranelift back end (DESIGN section 16.2): the same checked module, lowered to
//! Cranelift IR and emitted as an object file.
//!
//! It occupies the same phase as `cgen`, where the design places the boundary, and
//! consumes the same checked AST and `sema` types, so a program is a program
//! whichever back end takes it. What differs is what the *backend* decides:
//! integer arithmetic wraps because the IR defines it that way (which is what
//! `-fwrapv` buys the C path), the shifts follow the report rather than the IR's
//! masking, traversal order is the source's because there is no C compiler to
//! reorder it, and every check is a call into the runtime, which stays C
//! (DESIGN section 16.2 records the interface that fixes).
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

use syntax::ast::{
    BinOp, CaseArm, CaseLabel, Designator, Expr, ExprKind, Module, Proc, Selector, Span, Stmt,
    StmtKind, UnOp,
};
use cgen::cgen::{mangle, span_of_type};
use sema::const_eval;
use crate::layout::{Layout, Layouts};
use sema::sema::{span_key, Sema};
use sema::types::{Basic, Type, TypeId, Types};
use sema::Diagnostic;

/// A procedure's frame holds storage that nested procedures access through
/// static links (DESIGN section 9.7, the C back end's `F_<path>` struct). Its
/// first word is the static link. `sl` points to the parent's frame; a symbol
/// one level up uses that frame directly.
struct Frame {
    /// The lexical path (`Outer__Inner`), which nested symbols are named with.
    path: String,
    level: usize,
    /// Every parameter and local, by its offset in this frame.
    locals: HashMap<String, (u64, TypeId)>,
    size: u64,
}

/// A formal parameter as the checker flattened it, and how the C back end passes
/// it (DESIGN section 9.7). A value of a basic type is the value itself; a `VAR`
/// parameter is the caller's address; a *structured* value parameter is the
/// address of a copy the caller made; an open array is a pointer *and* a
/// hidden length (report section 10.1).
#[derive(Clone)]
struct Formal {
    name: String,
    ty: TypeId,
    by_var: bool,
}

impl Formal {
    fn open(&self, types: &Types) -> Option<TypeId> {
        match types.get(self.ty) {
            Type::OpenArray { elem } => Some(*elem),
            _ => None,
        }
    }

    fn structured(&self, types: &Types) -> bool {
        matches!(types.get(self.ty), Type::Record { .. } | Type::Array { .. })
    }
}

/// `INTEGER` and `BOOLEAN` are one IR type here (DESIGN section 9.2: an `int32_t`,
/// with 0/1 for truth values, so `ORD(p)` is the C value).
const I32: ir::Type = types::I32;

/// The width of an `INTEGER`, and so the stride of an array of them.
const WORD: u64 = 4;

/// A trap message this back end reports matches the C back end byte for byte.
/// Runtime helpers report arithmetic-check messages; the emitter makes these
/// two decisions (DESIGN section 9.4's table, and the corpus's adversarial mode
/// compares the text and byte).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Msg {
    /// `ASSERT` (report section 10.2, DESIGN section 9.6).
    Assertion,
    /// The report leaves a `CASE` with no matching arm undefined. W2.5's
    /// decision, also used by the C back end, is to trap at the source location.
    CaseDefault,
    /// A *type*-`CASE` with no matching label (DESIGN section 9.3's trapping
    /// default, which the C back end has always had).
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

/// One runtime entry point the generated code calls, from DESIGN section 9.4:
/// the four checks and traps in this fragment. Each points to a `static inline`
/// helper of the same name in `oberon.h`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Rt {
    Index,
    Div,
    Mod,
    Trap,
    /// `rt_is(descriptor, target)`: whether an object is of a type (section 9.3's
    Is,
    /// `rt_rec_copy(dst, src, n, self)`: a record copy that leaves the
    /// destination's own descriptor in place.
    RecCopy,
    /// `rt_new(size, descriptor)`: the only route to the heap, so the collector
    /// sees every object and a failed allocation is NIL (report section 6.4).
    New,
    /// `ob_rt_guard_at(cond, at)`: a guard's report, whose byte is *in* the
    /// message the C back end bakes into a literal.
    GuardAt,
    /// to walk (DESIGN section 15.4).
    GcRoot,
    /// `rt_set(a, b)`: the mask represented by a set constructor `{a .. b}` (section 9.5).
    Set,
    /// `rt_strcopy`/`rt_charcmp`: the bounded CHAR-array operations (a literal
    /// into an array, and the relations).
    StrCopy,
    CharCmp,
    /// `rt_pack`/`rt_unpk`: the exponent scaling from report section 10.2; both
    /// take the storage of their operands.
    Pack,
    Unpk,
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
            Rt::Set => "ob_rt_set",
            Rt::StrCopy => "ob_rt_strcopy",
            Rt::CharCmp => "ob_rt_charcmp",
            Rt::Pack => "ob_rt_pack",
            Rt::Unpk => "ob_rt_unpk",
        }
    }
}

/// Lower one checked module to an object file, or report which construct it cannot
/// lower yet, with that construct's span, so the driver prints
/// it like any other failure.
pub fn emit_object<'a>(
    module: &'a Module,
    types: &'a Types,
    sema: &'a Sema<'a>,
    checks: cgen::cgen::Checks,
    opt: &str,
) -> Result<Vec<u8>, Vec<Diagnostic>> {
    build(module, types, sema, checks, opt).map_err(|d| vec![d])
}

fn build<'a>(
    module: &'a Module,
    types: &'a Types,
    sema: &'a Sema<'a>,
    checks: cgen::cgen::Checks,
    opt: &str,
) -> Result<Vec<u8>, Diagnostic> {
    let isa = host_isa(opt)?;
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

/// The host's target ISA and the optimisation level for the design's comparison.
/// `OptLevel` defaults to `none` (DESIGN section 16), so an unoptimised back end
/// would have been measured against the C path's `-O2`.
///
/// `ObjectBuilder` chooses ELF from the ISA's triple. The design's second back
/// end emits ELF; a host with another format gets an error instead of an object
/// this code cannot produce.
///
/// The caller's `-Oopt=...` setting, normalised to Cranelift's spelling.
/// `speed` is this compiler's default. The C back end goes through `gcc -O2`;
/// Cranelift defaults to `none`, chosen for fast compiles. Matching settings
/// keeps the back ends comparable (DESIGN section 16, PLAN.md W9.7).
fn opt_level(name: &str) -> Result<&'static str, Diagnostic> {
    match name {
        "none" => Ok("none"),
        "speed" => Ok("speed"),
        "speed_and_size" | "speed-and-size" => Ok("speed_and_size"),
        other => Err(host(format!(
            "`-Oopt={other}` names no optimisation level; expected none, speed or speed_and_size"
        ))),
    }
}

fn host_isa(opt: &str) -> Result<cranelift_codegen::isa::OwnedTargetIsa, Diagnostic> {
    let mut flags = settings::builder();
    flags
        .set("opt_level", opt_level(opt)?)
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
    // Layout is the same at every optimisation level. This path only prints
    // sizes and offsets, so it asks for the ISA without choosing a level.
    let isa = host_isa("speed").map_err(|d| d.message)?;
    Ok(u64::from(isa.frontend_config().pointer_bytes()))
}

/// Write a number into a data object's bytes using native endianness and its
/// width. The ELF check above confirms that the target is this host. This
/// matches the value `cc` writes and uses the width of a `size_t` field.
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

/// Before its builder exists, a body needs declarations for string literals
/// and symbols from other modules. Declare imported procedures as function
/// references and imported variables as data references before building the
/// function, as Cranelift requires (DESIGN section 16.2).
#[derive(Default)]
struct Uses {
    strings: Vec<(u32, String)>,
    /// `(symbol, the callee's procedure type or the variable's type, is a call)`.
    imports: Vec<(String, TypeId, bool)>,
}

/// Each string literal's starting byte. Declare literals used as pointers,
/// including for `ARRAY OF CHAR` and character-array comparisons, as data objects
/// before the builder exists. A literal used only as `CHAR`, like a trap message,
/// does not need a data object.
fn collect_strings(sema: &Sema<'_>, body: &[Stmt], out: &mut Vec<(u32, String)>) {
    for s in body {
        collect_strings_stmt(sema, s, out);
    }
}

/// Locals whose storage is passed to another routine: a `VAR` actual or
/// `PACK`/`UNPK` operands, which take addresses (report sections 10.1 and 10.2).
/// A register value has no address to pass, so these locals use frame slots as
/// in the C back end.
fn address_taken(
    sema: &Sema<'_>,
    module: &Module,
    body: &[Stmt],
    proc_formals: &HashMap<String, Vec<Formal>>,
    out: &mut std::collections::BTreeSet<String>,
) {
    for s in body {
        match &s.kind {
            StmtKind::Assign { value, .. } => address_taken_expr(sema, module, value, proc_formals, out),
            StmtKind::Call(d) => address_taken_call(sema, module, d, proc_formals, out),
            StmtKind::If { arms, els } => {
                for (c, b) in arms {
                    address_taken_expr(sema, module, c, proc_formals, out);
                    address_taken(sema, module, b, proc_formals, out);
                }
                if let Some(b) = els {
                    address_taken(sema, module, b, proc_formals, out);
                }
            }
            StmtKind::While { arms } => {
                for (c, b) in arms {
                    address_taken_expr(sema, module, c, proc_formals, out);
                    address_taken(sema, module, b, proc_formals, out);
                }
            }
            StmtKind::Repeat { body, until } => {
                address_taken(sema, module, body, proc_formals, out);
                address_taken_expr(sema, module, until, proc_formals, out);
            }
            StmtKind::For {
                from, to, by, body, ..
            } => {
                address_taken_expr(sema, module, from, proc_formals, out);
                address_taken_expr(sema, module, to, proc_formals, out);
                if let Some(k) = by {
                    address_taken_expr(sema, module, k, proc_formals, out);
                }
                address_taken(sema, module, body, proc_formals, out);
            }
            StmtKind::Case { subject, arms } => {
                address_taken_expr(sema, module, subject, proc_formals, out);
                for arm in arms {
                    address_taken(sema, module, &arm.body, proc_formals, out);
                }
            }
            StmtKind::Return(e) => {
                if let Some(e) = e {
                    address_taken_expr(sema, module, e, proc_formals, out);
                }
            }
            StmtKind::Exit => {}
        }
    }
}

fn address_taken_call(
    sema: &Sema<'_>,
    module: &Module,
    d: &Designator,
    proc_formals: &HashMap<String, Vec<Formal>>,
    out: &mut std::collections::BTreeSet<String>,
) {
    let args = match d.sels.last() {
        Some(Selector::Call(args)) => args.clone(),
        _ => Vec::new(),
    };
    // `SYSTEM.ADR(x)` takes x's storage (report section 11.1), so x must have one.
    if d.base.len() == 2 && d.base[0] == "SYSTEM" && d.base[1] == "ADR" {
        for a in &args {
            if let ExprKind::Designator(d) = &a.kind {
                if d.sels.is_empty() {
                    out.insert(d.base[0].clone());
                }
            }
        }
    }
    if d.base.len() == 1 {
        let name = d.base[0].clone();
        if name == "PACK" || name == "UNPK" {
            for a in &args {
                if let ExprKind::Designator(d) = &a.kind {
                    if d.sels.is_empty() {
                        out.insert(d.base[0].clone());
                    }
                }
            }
        }
        let formals = proc_formals
            .get(&name)
            .cloned()
            .or_else(|| {
                sema.call_types
                    .get(&span_key(d.span))
                    .and_then(|t| match sema.types.get(*t) {
                        Type::Proc { params, .. } => Some(
                            params
                                .iter()
                                .enumerate()
                                .map(|(i, pm)| Formal {
                                    name: format!("a{i}"),
                                    ty: pm.ty,
                                    by_var: pm.by_var,
                                })
                                .collect(),
                        ),
                        _ => None,
                    })
            })
            .unwrap_or_default();
        for (a, f) in args.iter().zip(formals.iter()) {
            if f.by_var {
                if let ExprKind::Designator(d) = &a.kind {
                    if d.sels.is_empty() {
                        out.insert(d.base[0].clone());
                    }
                }
            }
        }
    }
    let _ = module;
    for a in &args {
        address_taken_expr(sema, module, a, proc_formals, out);
    }
}

fn address_taken_expr(
    sema: &Sema<'_>,
    module: &Module,
    e: &Expr,
    proc_formals: &HashMap<String, Vec<Formal>>,
    out: &mut std::collections::BTreeSet<String>,
) {
    match &e.kind {
        ExprKind::Designator(d) => address_taken_call(sema, module, d, proc_formals, out),
        ExprKind::Unary(_, x) => address_taken_expr(sema, module, x, proc_formals, out),
        ExprKind::Binary(_, l, r) => {
            address_taken_expr(sema, module, l, proc_formals, out);
            address_taken_expr(sema, module, r, proc_formals, out);
        }
        ExprKind::TypeTest(x, _) => address_taken_expr(sema, module, x, proc_formals, out),
        ExprKind::Set(elems) => {
            for el in elems {
                address_taken_expr(sema, module, &el.lo, proc_formals, out);
                if let Some(h) = &el.hi {
                    address_taken_expr(sema, module, h, proc_formals, out);
                }
            }
        }
        _ => {}
    }
}

fn uses_of(sema: &Sema<'_>, types: &Types, module: &Module, body: &[Stmt]) -> Uses {
    let mut uses = Uses::default();
    collect_strings(sema, body, &mut uses.strings);
    collect_imports(sema, types, module, body, &mut uses);
    uses.strings.sort_unstable();
    uses.strings.dedup();
    uses.imports.sort_by(|a, b| a.0.cmp(&b.0));
    uses.imports.dedup_by(|a, b| a.0 == b.0);
    uses
}

/// The alias a module was imported under resolves to the module it names
/// (report section 11).
fn alias_of(module: &Module, alias: &str) -> Option<String> {
    module
        .imports
        .iter()
        .find(|i| i.alias == alias)
        .map(|i| i.module.clone())
}

fn collect_imports(
    sema: &Sema<'_>,
    types: &Types,
    module: &Module,
    body: &[Stmt],
    uses: &mut Uses,
) {
    for s in body {
        match &s.kind {
            StmtKind::Assign { target, value } => {
                collect_imports_designator(sema, types, module, target, false, uses);
                collect_imports_expr(sema, types, module, value, uses);
            }
            // A statement-level designator *is* a call (a parameterless one has
            // no `Call` selector to say so).
            StmtKind::Call(d) => collect_imports_designator(sema, types, module, d, true, uses),
            StmtKind::If { arms, els } => {
                for (c, b) in arms {
                    collect_imports_expr(sema, types, module, c, uses);
                    collect_imports(sema, types, module, b, uses);
                }
                if let Some(b) = els {
                    collect_imports(sema, types, module, b, uses);
                }
            }
            StmtKind::While { arms } => {
                for (c, b) in arms {
                    collect_imports_expr(sema, types, module, c, uses);
                    collect_imports(sema, types, module, b, uses);
                }
            }
            StmtKind::Repeat { body, until } => {
                collect_imports(sema, types, module, body, uses);
                collect_imports_expr(sema, types, module, until, uses);
            }
            StmtKind::For {
                from, to, by, body, ..
            } => {
                collect_imports_expr(sema, types, module, from, uses);
                collect_imports_expr(sema, types, module, to, uses);
                if let Some(k) = by {
                    collect_imports_expr(sema, types, module, k, uses);
                }
                collect_imports(sema, types, module, body, uses);
            }
            StmtKind::Case { subject, arms } => {
                collect_imports_expr(sema, types, module, subject, uses);
                for arm in arms {
                    collect_imports(sema, types, module, &arm.body, uses);
                }
            }
            StmtKind::Return(e) => {
                if let Some(e) = e {
                    collect_imports_expr(sema, types, module, e, uses);
                }
            }
            StmtKind::Exit => {}
        }
    }
}

fn collect_imports_expr(
    sema: &Sema<'_>,
    types: &Types,
    module: &Module,
    e: &Expr,
    uses: &mut Uses,
) {
    match &e.kind {
        ExprKind::Designator(d) => collect_imports_designator(sema, types, module, d, false, uses),
        ExprKind::Unary(_, x) => collect_imports_expr(sema, types, module, x, uses),
        ExprKind::Binary(_, l, r) => {
            collect_imports_expr(sema, types, module, l, uses);
            collect_imports_expr(sema, types, module, r, uses);
        }
        ExprKind::TypeTest(x, _) => collect_imports_expr(sema, types, module, x, uses),
        ExprKind::Set(elems) => {
            for el in elems {
                collect_imports_expr(sema, types, module, &el.lo, uses);
                if let Some(h) = &el.hi {
                    collect_imports_expr(sema, types, module, h, uses);
                }
            }
        }
        _ => {}
    }
}

fn collect_imports_designator(
    sema: &Sema<'_>,
    types: &Types,
    module: &Module,
    d: &Designator,
    statement_call: bool,
    uses: &mut Uses,
) {
    if d.base.len() == 2 && sema.const_at(d.span).is_none() {
        if let Some(m) = alias_of(module, &d.base[0]) {
            let symbol = format!("{}__{}", m, mangle(&d.base[1]));
            let called = statement_call || matches!(d.sels.last(), Some(Selector::Call(_)));
            // The checker recorded the whole designator's type where it occurs:
            // a call's type at the call site, or a variable's at its use. The C
            // back end reads it the same way; imported variables are not in its
            // type environment.
            // A call's type is recorded at its designator only when it has a
            // `Call` selector. A parameterless call written without parentheses
            // has none, and the checker already established that it takes no
            // parameters (report section 9.2). `TypeId(0)` means no signature is
            // available, so the declaration uses an empty one.
            let ty = if called {
                sema.call_types
                    .get(&span_key(d.span))
                    .copied()
                    .unwrap_or(TypeId(0))
            } else {
                match sema.type_at(d.span, types) {
                    Some(t) => t,
                    None => return,
                }
            };
            uses.imports.push((symbol, ty, called));
        }
    }
    for sel in &d.sels {
        match sel {
            Selector::Index(ix) => {
                for e in ix {
                    collect_imports_expr(sema, types, module, e, uses);
                }
            }
            Selector::Call(args) | Selector::Args(Some(args)) => {
                for e in args {
                    collect_imports_expr(sema, types, module, e, uses);
                }
            }
            _ => {}
        }
    }
}

fn collect_strings_stmt(sema: &Sema<'_>, s: &Stmt, out: &mut Vec<(u32, String)>) {
    match &s.kind {
        StmtKind::Assign { target, value } => {
            collect_strings_designator(sema, target, out);
            collect_strings_expr(sema, value, out);
        }
        StmtKind::Call(d) => collect_strings_designator(sema, d, out),
        StmtKind::If { arms, els } => {
            for (c, body) in arms {
                collect_strings_expr(sema, c, out);
                collect_strings(sema, body, out);
            }
            if let Some(body) = els {
                collect_strings(sema, body, out);
            }
        }
        StmtKind::While { arms } => {
            for (c, body) in arms {
                collect_strings_expr(sema, c, out);
                collect_strings(sema, body, out);
            }
        }
        StmtKind::Repeat { body, until } => {
            collect_strings(sema, body, out);
            collect_strings_expr(sema, until, out);
        }
        StmtKind::For {
            from, to, by, body, ..
        } => {
            collect_strings_expr(sema, from, out);
            collect_strings_expr(sema, to, out);
            if let Some(k) = by {
                collect_strings_expr(sema, k, out);
            }
            collect_strings(sema, body, out);
        }
        StmtKind::Case { subject, arms } => {
            collect_strings_expr(sema, subject, out);
            for arm in arms {
                collect_strings(sema, &arm.body, out);
            }
        }
        StmtKind::Return(e) => {
            if let Some(e) = e {
                collect_strings_expr(sema, e, out);
            }
        }
        StmtKind::Exit => {}
    }
}

fn collect_strings_designator(sema: &Sema<'_>, d: &Designator, out: &mut Vec<(u32, String)>) {
    for sel in &d.sels {
        match sel {
            Selector::Index(ix) => {
                for e in ix {
                    collect_strings_expr(sema, e, out);
                }
            }
            Selector::Call(args) | Selector::Args(Some(args)) => {
                for e in args {
                    collect_strings_expr(sema, e, out);
                }
            }
            Selector::Args(None) | Selector::Field(_) | Selector::Guard(_) | Selector::Deref => {}
        }
    }
}

fn collect_strings_expr(sema: &Sema<'_>, e: &Expr, out: &mut Vec<(u32, String)>) {
    match &e.kind {
        ExprKind::Str(text) => out.push((e.span.lo, text.clone())),
        ExprKind::Designator(d) => {
            if d.sels.is_empty() {
                if let Some(const_eval::Const::Str(text)) = sema.const_at(e.span) {
                    out.push((e.span.lo, text));
                }
            }
            collect_strings_designator(sema, d, out);
        }
        ExprKind::Unary(_, x) => collect_strings_expr(sema, x, out),
        ExprKind::Binary(_, l, r) => {
            collect_strings_expr(sema, l, out);
            collect_strings_expr(sema, r, out);
        }
        ExprKind::TypeTest(x, _) => collect_strings_expr(sema, x, out),
        ExprKind::Set(elems) => {
            for el in elems {
                collect_strings_expr(sema, &el.lo, out);
                if let Some(h) = &el.hi {
                    collect_strings_expr(sema, h, out);
                }
            }
        }
        ExprKind::Int(_) | ExprKind::Real(_) | ExprKind::Char(_) | ExprKind::Bool(_) | ExprKind::Nil => {}
    }
}

fn string_constant(sema: &Sema<'_>, e: &Expr) -> Option<String> {
    match &e.kind {
        ExprKind::Str(text) => Some(text.clone()),
        _ => match sema.const_at(e.span) {
            Some(const_eval::Const::Str(text)) => Some(text),
            _ => None,
        },
    }
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

/// How large a callee may be and still be inlined (PLAN.md W9.4).
const INLINE_INSTRUCTION_LIMIT: usize = 64;

/// Cranelift's inliner calls this policy for each candidate. The library has no
/// heuristics: `cranelift-codegen/src/inline.rs` calls itself "inlining as a
/// library" because compilation is per-function and it cannot see the call graph.
struct Policy<'a> {
    /// What is being inlined *into*, so a call to itself is never inlined.
    caller: u32,
    /// Every body this back end has lowered, keyed by its `FuncId`.
    bodies: &'a HashMap<u32, ir::Function>,
    limit: usize,
}

impl<'a> cranelift_codegen::inline::Inline for Policy<'a> {
    fn inline(
        &mut self,
        caller: &ir::Function,
        _inst: ir::Inst,
        _opcode: ir::Opcode,
        callee: ir::FuncRef,
        _args: &[ir::Value],
    ) -> cranelift_codegen::inline::InlineCommand<'_> {
        use cranelift_codegen::inline::InlineCommand;
        // The name identifies module procedures, but runtime imports are also
        // `User`-named. Only module procedures have bodies.
        let id = match caller.dfg.ext_funcs[callee].name {
            ir::ExternalName::User(r) => caller.params.user_named_funcs()[r].index,
            _ => return InlineCommand::KeepCall,
        };
        if id == self.caller {
            return InlineCommand::KeepCall;
        }
        // `visit_callee: false`: the inlined body is not walked for further
        // inlining, which bounds recursion without the call graph.
        match self.bodies.get(&id) {
            Some(body) if body.dfg.num_insts() <= self.limit => InlineCommand::Inline {
                callee: std::borrow::Cow::Borrowed(body),
                visit_callee: false,
            },
            _ => InlineCommand::KeepCall,
        }
    }
}

/// A map's entries in a fixed order.
///
/// Every function must declare module data in the same sequence. A `GlobalValue`
/// `Symbol` name is a `User` name indexing the function's name table, and
/// `Context::inline` copies it into the caller. `HashMap` iteration order varies
/// across functions and runs, so impose one sequence (PLAN.md W9.4).
fn sorted<K: Ord, V>(map: HashMap<K, V>) -> Vec<(K, V)> {
    let mut entries: Vec<(K, V)> = map.into_iter().collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

struct Backend<'a> {
    /// The module, for the type declarations a label or a guard names.
    module: &'a Module,
    module_name: String,
    types: &'a Types,
    sema: &'a Sema<'a>,
    checks: cgen::cgen::Checks,
    obj: ObjectModule,
    /// The IR type and byte width of an address, used for symbol loads and
    /// runtime pointer arguments.
    ptr: ir::Type,
    ptr_bytes: u64,
    /// The C ABI's layout of every type the module stores: an array's element
    /// stride and (for records, PLAN.md W9.2b) a field's offset come from here.
    /// Computed once for the module rather than per body, because it is a
    /// property of the types and not of a function.
    layouts: HashMap<TypeId, Layout>,
    /// The module's record fields, for a `Field` selector's offset.
    records: HashMap<TypeId, crate::layout::RecordLayout>,
    /// The descriptor of every record type the module names (DESIGN section 9.3),
    /// plus the collector's field map for each record that reaches a pointer.
    descs: HashMap<TypeId, DataId>,
    maps: HashMap<TypeId, DataId>,
    nfields: HashMap<TypeId, u32>,
    /// A module variable's own map, when its type reaches a pointer: what
    /// `__init` registers with the collector.
    root_maps: HashMap<String, DataId>,
    /// The frames of the procedures being lowered, outermost first: what a
    /// nested procedure's enclosing accesses and links are computed from.
    frames: Vec<Frame>,
    /// Each procedure's lexical level (1 at module level) and path, for the link
    /// a nested call passes and the name it is emitted under.
    proc_level: HashMap<String, usize>,
    /// Procedures declared by the module, keyed by emitted symbol, with their
    /// formals. Calls need these if the checker has no call-site type. In
    /// particular, a parameterless call without parentheses has no `Call` selector.
    procs: HashMap<String, FuncId>,
    proc_formals: HashMap<String, Vec<Formal>>,
    /// Every string literal in the module, in one order for every function, and
    /// the bodies this back end has lowered, by `FuncId`, for the inliner.
    ///
    /// Inlining requires this order. `cranelift-module` names declarations with
    /// a per-function counter, so a `UserExternalName` only has meaning beside
    /// the function that produced it. The inliner translates names by value
    /// when splicing in a body. Functions must declare the same data in the same
    /// order so a callee's literal resolves to the right caller entry (PLAN.md W9.4).
    module_strings: Vec<(u32, String)>,
    /// Each string's data object is declared and defined once. The per-function
    /// pass adds `GlobalValue`s for existing objects instead of redeclaring them.
    string_data: HashMap<u32, DataId>,
    bodies: HashMap<u32, ir::Function>,
    /// The module's variables, as data objects.
    vars: HashMap<String, DataId>,
    /// The type of each module variable, including its array length and element
    /// type, which are not stored in the data object.
    var_ty: HashMap<String, TypeId>,
    /// The runtime entry points and the registry call, declared for the module.
    rt: HashMap<Rt, FuncId>,
    register: FuncId,
    /// The messages this back end's own traps carry (`Msg`).
    msgs: HashMap<Msg, DataId>,
    /// `run` initializes these fields in order. They are read only afterward;
    /// otherwise an early read could use an invalid symbol.
    /// The module's own name for `ob_register_commands` and its command table
    /// (report section 11).
    name_data: DataId,
    commands: DataId,
    /// The module body's entry point and the entry point the driver's loader
    /// calls. Both are exported as the module interface (DESIGN section 9.8).
    init: FuncId,
    load: FuncId,
}

impl<'a> Backend<'a> {
    fn new(
        module: &'a Module,
        types: &'a Types,
        sema: &'a Sema<'a>,
        checks: cgen::cgen::Checks,
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
            frames: Vec::new(),
            proc_level: HashMap::new(),
            procs: HashMap::new(),
            proc_formals: HashMap::new(),
            module_strings: Vec::new(),
            string_data: HashMap::new(),
            bodies: HashMap::new(),
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
        // Collect module string literals once and declare each in every function
        // in this order. `Context::inline` copies each callee's `GlobalValueData`,
        // including a `Symbol`'s `User` name. That name indexes the function's
        // name table, so each function must declare data in the same order or an
        // inlined body's literal could resolve to an unrelated caller entry.
        {
            let mut all = Vec::new();
            for p in &module.decls.procs {
                collect_strings(self.sema, &p.body, &mut all);
            }
            collect_strings(self.sema, &module.body, &mut all);
            all.sort_unstable();
            all.dedup();
            self.module_strings = all;
            // Declared and defined here, once, so the per-function pass below only
            // hands out `GlobalValue`s for objects that already exist.
            for (at, text) in &self.module_strings.clone() {
                let id = self.declare_string(&format!("str_{at}"), text)?;
                self.string_data.insert(*at, id);
            }
        }
        // 0. The C ABI layout for every type the module stores (DESIGN section 16,
        //    `layout.rs`): array strides and record member offsets come from here.
        self.compute_layouts(module)?;
        // 1. The back end's own trap messages: `ASSERT` and an unmatched `CASE`
        //    (W2.5). Other traps come from runtime helpers. Declare these whether
        //    or not the module reaches them, as the C back end does with its
        //    command table. An unreferenced string costs a few bytes of `.rodata`;
        //    checking first would require walking the body to avoid that cost.
        for msg in [Msg::Assertion, Msg::CaseDefault, Msg::CaseType, Msg::Nil] {
            let id = self.declare_string(msg.symbol(), msg.text())?;
            self.msgs.insert(msg, id);
        }
        // 2. Runtime entry points and the registry. A declaration is a symbol
        // table entry, not code. The linker allows unused entries, which mixed
        // C modules rely on, so declare them once for the module. Lowering
        // determines which entries the code calls.
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
            Rt::Set,
            Rt::StrCopy,
            Rt::CharCmp,
            Rt::Pack,
            Rt::Unpk,
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
        // The module's record types, whose descriptors and field maps the
        // collector uses (section 15.2), followed by the variables' own maps.
        self.declare_descriptors(module)?;
        self.name_data = self.declare_string("module_name", &module.name)?;
        self.init = self.declare_function(&self.symbol("init"), true, &[], None, span)?;
        self.load = self.declare_function(&self.symbol("load"), true, &[], None, span)?;
        for p in &module.decls.procs {
            self.declare_proc(p)?;
        }
        // 4. Declare the command table before lowering any body. It names
        //    procedures from step 3, and step 5 passes it to the registry.
        self.emit_commands(module)?;
        // 5. Lower procedure bodies, the module body run by loading, and the
        //    entry point that performs the load.
        for p in &module.decls.procs {
            self.lower_proc(p)?;
        }
        self.lower_function(
            self.init,
            &[],
            &[],
            None,
            &module.body,
            span,
            String::new(),
            None,
            &[],
        )?;
        self.lower_load(module)?;
        Ok(())
    }

    /// Ask `layout.rs` for every type's layout. Lowering reads the resulting
    /// table instead of walking the type graph. The walk recurses through array
    /// elements and record bases, starting from declared types, variables,
    /// parameters, and locals.
    fn compute_layouts(&mut self, module: &Module) -> Result<(), Diagnostic> {
        let mut layouts = Layouts::new(self.types, self.ptr_bytes);
        // Predeclared types, which `SIZE` can name without a declaration
        // (report section 11.1's table).
        for t in [
            self.types.integer(),
            self.types.boolean(),
            self.types.char(),
            self.types.byte(),
            self.types.real(),
            self.types.set(),
        ] {
            layouts.of(t);
        }
        for decl in &module.decls.types {
            if let Some(t) = self.sema.type_at(span_of_type(&decl.ty), self.types) {
                layouts.of(t);
            }
        }
        for v in &module.decls.vars {
            layouts.of(self.decl_type(&v.ty));
        }
        for p in &module.decls.procs {
            let (formals, result) = self.formals(p)?;
            for f in &formals {
                layouts.of(f.ty);
                if let Some(elem) = f.open(self.types) {
                    layouts.of(elem);
                }
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

    /// Map entries identify pointers inside a region starting at `off` (DESIGN
    /// section 15.2, the C back end's `region_fields`). A pointer field uses one
    /// slot. Pointer arrays use consecutive slots; record-array elements each
    /// have a descriptor and map. A record field is its own region. Words,
    /// `CHAR`s, and procedure values have no object pointers. A pointer-free
    /// record has no map, so marking it costs one NULL test.
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

    /// Whether a region of this type contains a record. `desc_init` uses this
    /// check to find nested records and initialize their descriptor words.
    fn contains_record(&self, t: TypeId) -> bool {
        match self.types.get(t) {
            Type::Record { .. } => true,
            Type::Array { elem, .. } => self.contains_record(*elem),
            _ => false,
        }
    }

    /// The symbol for a record type, matching the C back end's struct name.
    /// A declared type keeps its Oberon name. A composite reached through a
    /// pointer without a declaration of its own gets a deterministic name.
    /// Unlike the C emitter's order-dependent anonymous-record numbers, such
    /// a name cannot be written by a client, so the back ends can differ here.
    fn type_symbol(&self, t: TypeId, names: &HashMap<TypeId, String>) -> String {
        // A type that came from an interface belongs to that module and is named
        // there (cgen's `type_c_name`); a declared one keeps its Oberon name; and
        // a composite reached through a pointer without a declaration of its own
        // gets a deterministic one.
        if let Some(q) = self.sema.qualified_name(t) {
            return mangle(&q);
        }
        names
            .get(&t)
            .cloned()
            .unwrap_or_else(|| format!("{}__Anon_{}", self.module_name, t.0))
    }

    /// Define a type's descriptor only when this module owns it. An interface's
    /// record type uses the descriptor defined by another module's object.
    /// Defining another `TD_` would give the type two descriptors, which `IS`,
    /// guards, and type-`CASE` read by address.
    fn owns_descriptor(&self, t: TypeId) -> bool {
        self.sema.qualified_name(t).is_none()
    }

    /// For each record type the module can name, declare its field map, name,
    /// and descriptor (`ObDesc`, DESIGN section 9.3). The C back end emits C
    /// text (`emit_type`). This path declares data objects. Both collectors walk
    /// the maps the same way, allowing mixed builds to share them.
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
            if !self.owns_descriptor(t) {
                // Another module's object defines it, and its field map with it:
                // this one only names the symbol.
                let id = self
                    .obj
                    .declare_data(&format!("TD_{symbol}"), Linkage::Import, false, false)
                    .map_err(|e| host(format!("{e}")))?;
                self.descs.insert(t, id);
                continue;
            }
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
        // Map entries are numbers. Descriptor references are relocations.
        let fld = crate::layout::ob_fld(self.ptr_bytes);
        for &t in &records {
            if !self.owns_descriptor(t) {
                continue;
            }
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
            if !self.owns_descriptor(t) {
                continue;
            }
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
        // a root that is not on the stack (DESIGN section 15.4), as in the C back end's
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
            // `rt_set(int32_t, int32_t) -> uint32_t`.
            Rt::Set => {
                sig.params.push(AbiParam::new(I32));
                sig.params.push(AbiParam::new(I32));
                sig.returns.push(AbiParam::new(I32));
            }
            // `rt_strcopy(char *, int32_t, const char *)`.
            Rt::StrCopy => {
                sig.params.push(AbiParam::new(self.ptr));
                sig.params.push(AbiParam::new(I32));
                sig.params.push(AbiParam::new(self.ptr));
            }
            // `rt_charcmp(const char *, int32_t, const char *, int32_t) -> int32_t`.
            Rt::CharCmp => {
                sig.params.push(AbiParam::new(self.ptr));
                sig.params.push(AbiParam::new(I32));
                sig.params.push(AbiParam::new(self.ptr));
                sig.params.push(AbiParam::new(I32));
                sig.returns.push(AbiParam::new(I32));
            }
            // `rt_pack(float *, int32_t)` and `rt_unpk(float *, int32_t *)`.
            Rt::Pack => {
                sig.params.push(AbiParam::new(self.ptr));
                sig.params.push(AbiParam::new(I32));
            }
            Rt::Unpk => {
                sig.params.push(AbiParam::new(self.ptr));
                sig.params.push(AbiParam::new(self.ptr));
            }
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
            // The index, division, and modulo checks take (value, limit, byte)
            // and return the value (DESIGN section 9.4).
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
        params: &[AbiParam],
        result: Option<TypeId>,
        span: Span,
    ) -> Result<FuncId, Diagnostic> {
        let mut sig = self.obj.make_signature();
        sig.params = params.to_vec();
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

    /// Another module's procedure, as a symbol this object imports: the C back
    /// end links against exactly this name (DESIGN section 9.7), and the ABI is the
    /// interface's.
    fn declare_import(
        &mut self,
        name: &str,
        params: &[AbiParam],
        result: Option<TypeId>,
        span: Span,
    ) -> Result<FuncId, Diagnostic> {
        let mut sig = self.obj.make_signature();
        sig.params = params.to_vec();
        if let Some(r) = result {
            sig.returns.push(AbiParam::new(self.scalar(r, span)?));
        }
        self.obj
            .declare_function(name, Linkage::Import, &sig)
            .map_err(|e| host(format!("{e}")))
    }

    /// The back end maps `INTEGER` and `BOOLEAN` to IR `i32`, and pointers to
    /// the target's address width. Reject other types here so diagnostics can
    /// name the source type instead of reporting an internal lowering failure.
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
    /// The frontend configuration, for the memory helpers the emitter uses.
    fn obj_config(&self) -> cranelift_codegen::isa::TargetFrontendConfig {
        self.obj.target_config()
    }

    fn type_align(&self, t: TypeId) -> u64 {
        self.layout(t).map(|l| l.align).unwrap_or(1)
    }

    /// The IR type a *value* of this Oberon type is held in, or `None` for a
    /// type that is a region rather than a value (a record, an array) or one
    /// this slice does not lower yet (`REAL`, `CHAR`, `SET`, PLAN.md W9.2c).
    fn value_type(&self, t: TypeId) -> Option<ir::Type> {
        match self.types.get(t) {
            // `int32_t`, and `BOOLEAN`'s 0/1 (DESIGN section 9.2).
            Type::Basic(Basic::Integer) | Type::Basic(Basic::Boolean) => Some(I32),
            // A `SET` is the C back end's `uint32_t` mask.
            Type::Basic(Basic::Set) => Some(I32),
            // `CHAR` and `BYTE` use one byte, like C's `char` and `uint8_t`.
            // Comparisons read them unsigned, as in C's `(unsigned char)` casts.
            Type::Basic(Basic::Char) | Type::Basic(Basic::Byte) => Some(types::I8),
            // The C back end's `float`.
            Type::Basic(Basic::Real) => Some(types::F32),
            Type::Pointer { .. } => Some(self.ptr),
            // Procedure values are code addresses. Only module-level procedures
            // can be assigned to one (report section 6.5), so no static link is
            // needed, matching the C back end's ABI.
            Type::Proc { .. } => Some(self.ptr),
            _ => None,
        }
    }

    /// Whether a type's values are *ordinal* in the C back end's sense, which is
    /// what decides a comparison's signedness: `CHAR` and `BYTE` are read
    /// unsigned (report section 8.2.4's ordinal order), and a `SET` is a `uint32_t`.
    fn unsigned(&self, t: TypeId) -> bool {
        matches!(
            self.types.get(t),
            Type::Basic(Basic::Char) | Type::Basic(Basic::Byte) | Type::Basic(Basic::Set)
        )
    }

    fn is_real(&self, t: TypeId) -> bool {
        matches!(self.types.get(t), Type::Basic(Basic::Real))
    }

    /// What a variable or a field of this type occupies, or a refusal. A record
    /// and an array are regions: their size is the layout's, and their *values*
    /// are not held in anything (a copy is `rt_rec_copy`, PLAN.md W9.2b's).
    fn storable(&self, t: TypeId, span: Span) -> Result<u64, Diagnostic> {
        match self.types.get(t) {
            Type::Basic(Basic::Integer) | Type::Basic(Basic::Boolean) => Ok(4),
            Type::Basic(Basic::Set) | Type::Basic(Basic::Real) => Ok(4),
            Type::Basic(Basic::Char) | Type::Basic(Basic::Byte) => Ok(1),
            Type::Pointer { .. } | Type::Proc { .. } => Ok(self.ptr_bytes),
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
    /// it costs one NULL test (DESIGN section 15.2). A pointer ends the walk, so
    /// the traversal cannot recurse indefinitely.
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

    /// Find a record's own field offset by walking its base chain. Inherited
    /// fields use the base's storage because a derived record embeds its base
    /// first.
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
    /// it (`TD_<Module>__<Name>`, DESIGN section 9.3), or a refusal for a type the
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
                // Both paths zero-initialize statics. An unassigned variable
                // reads as zero, the value the report leaves free.
                data.define_zeroinit(size);
                // Use the type's alignment, not a word's. The collector reads
                // pointer-bearing variables (`GCPtr`, records, or arrays), so
                // misalignment would fault in the collector. The C back end gets
                // alignment from the declared type.
                data.set_align(self.layout(ty).map(|l| l.align).unwrap_or(WORD));
                self.obj
                    .define_data(id, &data)
                    .map_err(|e| host(format!("{e}")))?;
                self.vars.insert(name.clone(), id);
                self.var_ty.insert(name.clone(), ty);
                // The collector's root, registered before the body can allocate
                // (section 15.4), and only when the variable's region can hold a
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
        let path = match self.frames.last() {
            Some(f) if f.level >= 1 => format!("{}__{}", f.path, p.name),
            _ => p.name.clone(),
        };
        let level = self.frames.last().map(|f| f.level + 1).unwrap_or(1);
        let (formals, result) = self.formals(p)?;
        let mut abi = Vec::new();
        if level > 1 {
            // A nested procedure takes the enclosing frame as a hidden first
            // parameter, whatever it does with it (DESIGN section 9.7: the C back end
            abi.push(AbiParam::new(self.ptr));
        }
        abi.extend(self.abi_params(&formals, p.name_span)?);
        let symbol = format!("{}__{}", self.module_name, mangle(&path));
        let id = self.declare_function(&symbol, p.export, &abi, result, p.name_span)?;
        // Keyed by the lexical path: two procedures called `inner` in different
        // parents are different procedures, and a call resolves by walking
        // outward from the caller's own path.
        self.procs.insert(path.clone(), id);
        self.proc_formals.insert(path.clone(), formals);
        self.proc_level.insert(path.clone(), level);
        // The nested ones, declared in the parent's frame's context: their
        // symbols carry the lexical path, which is what makes two procedures
        // called `inner` in different parents distinct.
        let frame = self.frame_of(p, level, &path);
        self.frames.push(frame);
        for q in &p.decls.procs {
            self.declare_proc(q)?;
        }
        self.frames.pop();
        Ok(())
    }

    /// A procedure with nested procedures stores the static link, parameters,
    /// and locals at aligned offsets in its frame (C back end's
    /// `struct F_<path>`). Without nested procedures, locals stay in registers
    /// and slots.
    fn frame_of(&self, p: &Proc, level: usize, path: &str) -> Frame {
        let mut locals = HashMap::new();
        let mut at = self.ptr_bytes;
        if !p.decls.procs.is_empty() {
            let (formals, _) = self.formals(p).unwrap_or_default();
            for f in &formals {
                let size = self.storable(f.ty, p.name_span).unwrap_or(self.ptr_bytes);
                let align = self.type_align(f.ty);
                at = crate::layout::round_up(at, align);
                locals.insert(f.name.clone(), (at, f.ty));
                at += size;
            }
            for v in &p.decls.vars {
                let ty = self.decl_type(&v.ty);
                let size = self.storable(ty, v.span).unwrap_or(0);
                let align = self.type_align(ty);
                for (name, _) in &v.names {
                    at = crate::layout::round_up(at, align);
                    locals.insert(name.clone(), (at, ty));
                    at += size;
                }
            }
        }
        Frame {
            path: path.to_string(),
            level,
            locals,
            size: crate::layout::round_up(at, self.ptr_bytes),
        }
    }

    /// A procedure's parameters, as the checker flattened them, and its result.
    fn formals(&self, p: &Proc) -> Result<(Vec<Formal>, Option<TypeId>), Diagnostic> {
        let sig = self
            .sema
            .signatures
            .get(&span_key(p.span))
            .ok_or_else(|| host(format!("no signature for `{}`", p.name)))?;
        let params = sig
            .params
            .iter()
            .map(|(name, pm)| Formal {
                name: name.clone(),
                ty: pm.ty,
                by_var: pm.by_var,
            })
            .collect();
        Ok((params, sig.result))
    }

    /// The C ABI for a formal (DESIGN section 9.7): an open array is a pointer and
    /// length. `VAR` and structured value parameters are addresses; everything else
    /// uses its own width.
    fn abi_params(&self, formals: &[Formal], span: Span) -> Result<Vec<AbiParam>, Diagnostic> {
        let mut out = Vec::new();
        for f in formals {
            if f.open(self.types).is_some() {
                out.push(AbiParam::new(self.ptr));
                out.push(AbiParam::new(I32));
            } else if f.by_var || f.structured(self.types) {
                out.push(AbiParam::new(self.ptr));
            } else {
                out.push(AbiParam::new(self.scalar(f.ty, span)?));
            }
        }
        Ok(out)
    }


    /// `Module__load` runs the module body and registers its commands, as the
    /// report describes module loading (section 11) and the driver does
    /// (DESIGN section 9.8).
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
        // A codegen failure here is a bug in *this* file. The module checked and
        // lowering wrote the IR. The driver receives the Debug form because
        // Debug form, because the `Display` of a verifier failure is the words
        // "Verifier errors" and nothing else: the instruction index and the type
        // it got wrong are the diagnosis.
        self.obj
            .define_function(id, &mut ctx)
            .map_err(|e| host(format!("{e:?}")))
    }

    /// The command table in report section 11 has an entry per *parameterless
    /// exported* procedure, then a NULL name to end it. Calling a parameterized
    /// procedure through `void (*)(void)` would be undefined. The C back end follows
    /// the same rule (DESIGN section 9.7).
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
        // The table needs relocations for addresses. A static linker cannot
        // write them into a section with no file bytes (`.bss`). The loader
        // would then read null entries and find no command.
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


    fn lower_proc(&mut self, p: &Proc) -> Result<(), Diagnostic> {
        let (formals, result) = self.formals(p)?;
        let mut locals = Vec::new();
        for v in &p.decls.vars {
            let ty = self.decl_type(&v.ty);
            for (lname, _) in &v.names {
                locals.push((lname.clone(), ty));
            }
        }
        let level = self.frames.last().map(|f| f.level + 1).unwrap_or(1);
        let path = match self.frames.last() {
            Some(f) => format!("{}__{}", f.path, p.name),
            None => p.name.clone(),
        };
        let id = *self
            .procs
            .get(&path)
            .expect("a declared procedure has a symbol");
        let body = p.body.clone();
        let frame = self.frame_of(p, level, &path);
        let has_frame = !p.decls.procs.is_empty();
        let nested = p.decls.procs.clone();
        self.lower_function(
            id,
            &formals,
            &locals,
            result,
            &body,
            p.name_span,
            path,
            if has_frame { Some(frame) } else { None },
            &nested,
        )
    }

    /// Lower a procedure's parameters and locals to SSA variables and its body
    /// to IR. Return from any path that reaches the end.
    #[allow(clippy::too_many_arguments)]
    fn lower_function(
        &mut self,
        id: FuncId,
        formals: &[Formal],
        locals: &[(String, TypeId)],
        result: Option<TypeId>,
        body: &[Stmt],
        span: Span,
        path: String,
        frame: Option<Frame>,
        nested: &[Proc],
    ) -> Result<(), Diagnostic> {
        let level = self.frames.last().map(|f| f.level + 1).unwrap_or(1);
        // Every symbol the body can name is declared *before* the builder
        // exists: a Cranelift function reference is part of the function, and
        // the frontend does not hand the `ir::Function` back once it holds it.
        // The string literals and other modules' symbols are data and function
        // references for the same reason.
        let uses = uses_of(self.sema, self.types, self.module, body);
        let mut string_ids = Vec::new();
        // Include every module string so all functions use the same name table
        // and the data objects declared once in `run`.
        for (at, _) in &self.module_strings.clone() {
            string_ids.push((*at, self.string_data[at]));
        }

        let mut ctx = self.obj.make_context();
        let mut sig = self.obj.make_signature();
        if level > 1 {
            // A nested procedure's first parameter is the enclosing frame
            // (DESIGN section 9.7).
            sig.params.push(AbiParam::new(self.ptr));
        }
        sig.params.extend(self.abi_params(formals, span)?);
        if let Some(r) = result {
            sig.returns.push(AbiParam::new(self.scalar(r, span)?));
        }
        ctx.func = ir::Function::with_name_signature(UserFuncName::user(0, id.as_u32()), sig);

        let mut frefs = HashMap::new();
        for (pname, pid) in self.procs.clone() {
            frefs.insert(pname, self.obj.declare_func_in_func(pid, &mut ctx.func));
        }
        // The module's data, in a *fixed* order in every function, and all of it
        // rather than only what this body names. The first matters because a
        // `GlobalValue` is copied into a caller verbatim, name included, and the
        // name indexes the *function's* table, so each function must declare the
        // same data in the same sequence (PLAN.md W9.4). A body-dependent set
        // would change that sequence.
        let mut gvs = HashMap::new();
        for (vname, vid) in sorted(self.vars.clone()) {
            gvs.insert(vname, self.obj.declare_data_in_func(vid, &mut ctx.func));
        }
        let mut rts = HashMap::new();
        for (rt, rid) in sorted(self.rt.clone()) {
            rts.insert(rt, self.obj.declare_func_in_func(rid, &mut ctx.func));
        }
        let mut msgs = HashMap::new();
        for (msg, mid) in sorted(self.msgs.clone()) {
            msgs.insert(msg, self.obj.declare_data_in_func(mid, &mut ctx.func));
        }
        let mut descs = HashMap::new();
        for (t, did) in sorted(self.descs.clone()) {
            descs.insert(t, self.obj.declare_data_in_func(did, &mut ctx.func));
        }
        let mut roots = HashMap::new();
        for (name, mid) in sorted(self.root_maps.clone()) {
            let n = self.region_fields(self.var_ty[&name], 0).len() as i32;
            roots.insert(name, (self.obj.declare_data_in_func(mid, &mut ctx.func), n));
        }
        let mut imported_ids = Vec::new();
        let mut imported_var_ids = Vec::new();
        for (symbol, ty, is_call) in &uses.imports {
            if !is_call {
                let did = self
                    .obj
                    .declare_data(symbol, Linkage::Import, true, false)
                    .map_err(|e| host(format!("{e}")))?;
                imported_var_ids.push((symbol.clone(), did, *ty));
                continue;
            }
            // A call the checker recorded a type for has the interface's ABI; a
            // parameterless call written without parentheses has none to read, and
            // an empty list of arguments. If the call has a result, it is
            // discarded, which the C ABI allows for the scalars this back end lowers.
            let (formals, result) = match self.types.get(*ty) {
                Type::Proc { params, result } => (
                    params
                        .iter()
                        .enumerate()
                        .map(|(i, pm)| Formal {
                            name: format!("a{i}"),
                            ty: pm.ty,
                            by_var: pm.by_var,
                        })
                        .collect::<Vec<Formal>>(),
                    *result,
                ),
                _ => (Vec::new(), None),
            };
            let abi = self.abi_params(&formals, span)?;
            let id = self.declare_import(symbol, &abi, result, span)?;
            imported_ids.push((symbol.clone(), id));
        }
        let mut strings_gv = HashMap::new();
        for (at, sid) in string_ids {
            strings_gv.insert(at, self.obj.declare_data_in_func(sid, &mut ctx.func));
        }
        let mut imported = HashMap::new();
        for (symbol, id) in imported_ids {
            imported.insert(symbol, self.obj.declare_func_in_func(id, &mut ctx.func));
        }
        let mut imported_vars = HashMap::new();
        let mut imported_var_tys = HashMap::new();
        for (symbol, id, ty) in imported_var_ids {
            imported_vars.insert(symbol.clone(), self.obj.declare_data_in_func(id, &mut ctx.func));
            imported_var_tys.insert(symbol, ty);
        }

        let mut fn_ctx = FunctionBuilderContext::new();
        {
            let mut bcx = FunctionBuilder::new(&mut ctx.func, &mut fn_ctx);
            let entry = bcx.create_block();
            bcx.append_block_params_for_function_params(entry);
            bcx.switch_to_block(entry);
            let values: Vec<ir::Value> = bcx.block_params(entry).to_vec();
            let mut at = 0;
            let sl = if level > 1 {
                at = 1;
                Some(values[0])
            } else {
                None
            };
            let mut vars = HashMap::new();
            let mut local_tys: HashMap<String, TypeId> = HashMap::new();
            let mut var_params: HashMap<String, (ir::Value, TypeId)> = HashMap::new();
            let mut open: HashMap<String, (ir::Value, ir::Value, TypeId)> = HashMap::new();
            let mut slots: HashMap<String, ir::Value> = HashMap::new();
            let mut regions: Vec<(TypeId, ir::Value)> = Vec::new();
            // The frame, when this procedure owns one: every parameter and local
            // lives in it, because a nested procedure reaches them through it.
            let frame_addr = match &frame {
                Some(f) => {
                    let align = self.ptr_bytes.trailing_zeros() as u8;
                    let slot = bcx.create_sized_stack_slot(StackSlotData::new(
                        StackSlotKind::ExplicitSlot,
                        f.size as u32,
                        align,
                    ));
                    let addr = bcx.ins().stack_addr(self.ptr, slot, 0);
                    bcx.emit_small_memset(
                        self.obj.target_config(),
                        addr,
                        0,
                        f.size,
                        align,
                        ir::MemFlagsData::new(),
                    );
                    // The link, first word: our own frame is what a *child* call
                    // passes on, and `NULL` at the top of a chain.
                    let link = match sl {
                        Some(s) => s,
                        None => bcx.ins().iconst(self.ptr, 0),
                    };
                    bcx.ins().store(ir::MemFlagsData::new(), link, addr, 0);
                    Some(addr)
                }
                None => None,
            };
            for f in formals {
                local_tys.insert(f.name.clone(), f.ty);
                if let Some(elem) = f.open(self.types) {
                    open.insert(f.name.clone(), (values[at], values[at + 1], elem));
                    at += 2;
                } else if f.by_var || f.structured(self.types) {
                    var_params.insert(f.name.clone(), (values[at], f.ty));
                    at += 1;
                } else if let (Some(addr), Some(fr)) = (frame_addr, &frame) {
                    // In the frame, at the offset the frame layout gave it.
                    let (off, _) = fr.locals[&f.name];
                    let here = bcx.ins().iadd_imm_s(addr, off as i64);
                    bcx.ins().store(ir::MemFlagsData::new(), values[at], here, 0);
                    slots.insert(f.name.clone(), here);
                    at += 1;
                } else {
                    let var = bcx.declare_var(self.scalar(f.ty, span)?);
                    bcx.def_var(var, values[at]);
                    vars.insert(f.name.clone(), var);
                    at += 1;
                }
            }
            let mut taken = std::collections::BTreeSet::new();
            address_taken(self.sema, self.module, body, &self.proc_formals, &mut taken);
            for (lname, ty) in locals {
                local_tys.insert(lname.clone(), *ty);
                if let (Some(addr), Some(fr)) = (frame_addr, &frame) {
                    let (off, _) = fr.locals[lname];
                    let here = bcx.ins().iadd_imm_s(addr, off as i64);
                    slots.insert(lname.clone(), here);
                    // A frame stores a descriptor word for each record local.
                    // The walk that initializes nested records visits these too.
                    if self.contains_record(*ty) {
                        regions.push((*ty, here));
                    }
                    continue;
                }
                if self.value_type(*ty).is_none() || taken.contains(lname) {
                    // These locals are regions or have their address passed to a
                    // routine (`VAR`, `PACK`, or `UNPK`). Both need addressable
                    // storage, so the frame assigns them slots.
                    let size = self.layout(*ty).map(|l| l.size).ok_or_else(|| {
                        not_yet(&format!("the local `{lname}`"), span)
                    })?;
                    if size > u64::from(u32::MAX) {
                        return Err(Diagnostic::error(
                            span,
                            format!(
                                "the local `{lname}` is {size} bytes; the clif back end's stack \
                                 slots are 32-bit"
                            ),
                        ));
                    }
                    let align = self.type_align(*ty).trailing_zeros() as u8;
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
            // The enclosing locals this body can name, and how far up they are:
            // `sl` points at the parent's frame, so a symbol one level up needs
            // no hop at all (DESIGN section 9.7).
            let mut outer = HashMap::new();
            for fr in &self.frames {
                let hops = level.saturating_sub(fr.level + 1);
                for (name, (off, ty)) in &fr.locals {
                    outer.insert(name.clone(), (hops, *off, *ty));
                }
            }
            let mut l = Lower::new(
                self, bcx, vars, slots, local_tys, var_params, open, strings_gv, imported,
                imported_vars, imported_var_tys, gvs, frefs, rts, msgs, descs, roots, outer,
                frame_addr, sl, level, path.clone(), result,
            );
            for (ty, addr) in regions {
                l.desc_init(ty, addr, span)?;
            }
            if id == self.init {
                l.init_preamble(span)?;
            }
            l.stmts(body)?;
            if !l.ended() {
                if result.is_some() {
                    let rty = self
                        .value_type(result.unwrap_or(TypeId(0)))
                        .unwrap_or(I32);
                    let zero = if rty == types::F32 {
                        l.bcx.ins().f32const(0.0)
                    } else {
                        l.bcx.ins().iconst(rty, 0)
                    };
                    l.bcx.ins().return_(&[zero]);
                } else {
                    l.bcx.ins().return_(&[]);
                }
            }
            l.bcx.seal_all_blocks();
            l.bcx.finalize(l.b.obj.target_config());
        }
        // The policy (DESIGN section 16, PLAN.md W9.4) adds this body to the map
        // before lowering later procedures, so their calls can inline it. The
        // policy refuses to inline recursive calls into the current procedure.
        self.bodies.insert(id.as_u32(), ctx.func.clone());
        let mut policy = Policy {
            caller: id.as_u32(),
            bodies: &self.bodies,
            limit: INLINE_INSTRUCTION_LIMIT,
        };
        ctx.inline(&mut policy).map_err(|e| host(format!("{e:?}")))?;
        if std::env::var_os("OB_TRACE_IR").is_some() {
            eprintln!("---- fn {}:\n{}", id.as_u32(), ctx.func.display());
        }

        self.obj
            .define_function(id, &mut ctx)
            .map_err(|e| host(format!("{e:?}")))?;
        // The nested procedures, with this frame on the stack: their bodies reach
        // it through the link they were handed, and their own frames push onto
        // the same stack.
        if let Some(f) = frame {
            self.frames.push(f);
            let mut failed = None;
            for q in nested {
                if let Err(e) = self.lower_proc(q) {
                    failed = Some(e);
                    break;
                }
            }
            self.frames.pop();
            if let Some(e) = failed {
                return Err(e);
            }
        }
        Ok(())
    }

    /// The type a declaration's `TypeExpr` denotes: the semantic pass recorded
    /// every type expression by span, and this reads it the same way the C back
    /// end does.
    fn decl_type(&self, t: &syntax::ast::TypeExpr) -> TypeId {
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

/// A designator is either a local value (an SSA variable) or an address. The
/// address can refer to a module variable, local region, field, element, or
/// heap object behind a pointer.
enum Place {
    Local(Variable, TypeId),
    Addr(ir::Value, TypeId),
}

fn place_ty(place: &Place) -> TypeId {
    match place {
        Place::Local(_, t) | Place::Addr(_, t) => *t,
    }
}

/// The IR for one procedure: SSA values for locals and data objects for module
/// variables, plus references used by the body.
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
    /// A `VAR` parameter, or a structured value parameter: what the body names is
    /// the storage the caller passed (the caller's own for a `VAR`, a copy of the
    /// actual for a value).
    var_params: HashMap<String, (ir::Value, TypeId)>,
    /// An open-array parameter stores the element pointer and element type. Its
    /// hidden length comes from the caller (report section 10.1).
    open: HashMap<String, (ir::Value, ir::Value, TypeId)>,
    /// The data object of every string literal the body mentions, by its byte.
    strings: HashMap<u32, ir::GlobalValue>,
    /// The procedures of other modules the body calls, by their symbol, and the
    /// variables of other modules it reads (`Oberon.Log` is one: an interface's
    /// variable is the defining module's storage).
    imported: HashMap<String, ir::FuncRef>,
    imported_vars: HashMap<String, ir::GlobalValue>,
    /// The type each of those variables has, as the interface declares it.
    imported_var_tys: HashMap<String, TypeId>,
    /// The narrowing a type-`CASE` arm gives the case variable (report section 9.5).
    narrow: Vec<HashMap<String, TypeId>>,
    /// The enclosing procedures' locals, by name, with the link count, frame
    /// offset, and type needed to reach each one.
    outer: HashMap<String, (usize, u64, TypeId)>,
    /// This procedure's own frame (when it owns one), and the static link it was
    /// handed (when it is nested).
    frame: Option<ir::Value>,
    sl: Option<ir::Value>,
    /// This procedure's lexical level, which is what a nested call's link is
    /// computed from, and its lexical path, which is what its symbols are keyed
    /// by.
    level: usize,
    path: String,
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
        var_params: HashMap<String, (ir::Value, TypeId)>,
        open: HashMap<String, (ir::Value, ir::Value, TypeId)>,
        strings: HashMap<u32, ir::GlobalValue>,
        imported: HashMap<String, ir::FuncRef>,
        imported_vars: HashMap<String, ir::GlobalValue>,
    // (the types of those variables travel with them)
    imported_var_tys: HashMap<String, TypeId>,
        gv: HashMap<String, ir::GlobalValue>,
        frefs: HashMap<String, ir::FuncRef>,
        rt: HashMap<Rt, ir::FuncRef>,
        msgs: HashMap<Msg, ir::GlobalValue>,
        descs: HashMap<TypeId, ir::GlobalValue>,
        roots: HashMap<String, (ir::GlobalValue, i32)>,
        outer: HashMap<String, (usize, u64, TypeId)>,
        frame: Option<ir::Value>,
        sl: Option<ir::Value>,
        level: usize,
        path: String,
        result: Option<TypeId>,
    ) -> Lower<'b, 'f, 'a> {
        Lower {
            b,
            bcx,
            vars,
            slots,
            local_tys,
            var_params,
            open,
            strings,
            imported,
            imported_vars,
            imported_var_tys,
            gv,
            frefs,
            rt,
            msgs,
            descs,
            roots,
            narrow: Vec::new(),
            outer,
            frame,
            sl,
            level,
            path,
            result,
            loops: Vec::new(),
        }
    }


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

    /// `d := e`. Lower the value first and the place second. If both sides
    /// could trap, report the value's trap. C leaves the order to the compiler
    /// (R23). IR imposes its own order.
    fn assign(&mut self, target: &Designator, value: &Expr) -> Result<(), Diagnostic> {
        let place = self.place(target, target.span)?;
        let ty = place_ty(&place);
        // Runtime helpers copy `CHAR` arrays (section 9.5). String values are
        // length-checked and zero-padded. Array copies move at most the
        // destination length and do not add a terminator (R14).
        if let Type::Array { len, elem } = self.b.types.get(ty) {
            if self.b.types.is_char(*elem) {
                let (len, _) = (*len, *elem);
                let (dst, _) = self.address_of(&place, target.span)?;
                if let Some(text) = string_constant(self.b.sema, value) {
                    let src = self.string_ptr(&text, value.span)?;
                    let n = self.bcx.ins().iconst(I32, i64::from(len));
                    let fref = self.rt_ref(Rt::StrCopy);
                    self.bcx.ins().call(fref, &[dst, n, src]);
                    return Ok(());
                }
                if matches!(&value.kind, ExprKind::Designator(_)) {
                    // A full-length array copy is a byte copy with no
                    // terminator (R14), matching the C back end's struct
                    // assignment. The design names `rt_charcopy`, but the
                    // header has no such helper.
                    let (src, _) = self.char_array_operand(value)?;
                    let size = len as u64;
                    self.bcx.emit_small_memory_copy(
                        self.b.obj_config(),
                        dst,
                        src,
                        size,
                        1,
                        1,
                        false,
                        ir::MemFlagsData::new(),
                    );
                    return Ok(());
                }
                return Err(not_yet(
                    "this value assigned to a character array",
                    value.span,
                ));
            }
        }
        // A record assignment copies the region. `rt_rec_copy` handles an
        // up-cast by moving fields and preserving the destination descriptor.
        // A base variable keeps its type (DESIGN section 9.3).
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


/// A local value is an SSA variable, as with a register. Other designators are
/// addresses in a module object, frame slot, field, element, or heap object.
    fn place(&mut self, d: &Designator, span: Span) -> Result<Place, Diagnostic> {
        // A qualified base is another module's variable, which this object
        // imports: its type is the interface's.
        if d.base.len() == 2 {
            let symbol = format!("{}__{}", d.base[0], mangle(&d.base[1]));
            let gv = *self.imported_vars.get(&symbol).ok_or_else(|| {
                not_yet(
                    &format!("the designator `{}`", d.base.join(".")),
                    span,
                )
            })?;
            let ty = *self.imported_var_tys.get(&symbol).ok_or_else(|| {
                not_yet(&format!("the type of `{}`", d.base.join(".")), span)
            })?;
            let mut place = Place::Addr(self.bcx.ins().symbol_value(self.b.ptr, gv), ty);
            for sel in &d.sels {
                place = self.select(place, sel, d.span)?;
            }
            return Ok(place);
        }
        let name = self.base_name(d)?;
        let mut place = if let Some((ptr, ty)) = self.var_params.get(name).copied() {
            // A `VAR` parameter uses the caller's storage. A structured value
            // parameter uses the address of the caller's copy.
            Place::Addr(ptr, self.narrowed(name, ty))
        } else if let Some((ptr, _len, elem)) = self.open.get(name).copied() {
            // An open array is its element pointer: the first selector must be an
            // index, which adds the checked stride to it.
            Place::Addr(ptr, self.narrowed(name, elem))
        } else if let Some(var) = self.vars.get(name).copied() {
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
        } else if let Some((hops, offset, ty)) = self.outer.get(name).copied() {
            // Walk the static links `hops` times to reach an enclosing local.
            // `sl` points to the parent's frame, so a parent local is one link up.
            let mut link = self
                .sl
                .ok_or_else(|| not_yet(&format!("the enclosing local `{name}`"), span))?;
            for _ in 0..hops {
                link = self
                    .bcx
                    .ins()
                    .load(self.b.ptr, ir::MemFlagsData::new(), link, 0);
            }
            Place::Addr(self.bcx.ins().iadd_imm_s(link, offset as i64), ty)
        } else {
            return Err(not_yet(&format!("the designator `{name}`"), span));
        };
        for sel in &d.sels {
            place = self.select(place, sel, d.span)?;
        }
        Ok(place)
    }

    /// The base name of a designator. A qualified name (`M.x`) needs another
    /// module's symbol when importing a lowered module (PLAN.md W9.2c).
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

    /// The narrowing a type-`CASE` arm gives a name (report section 9.5 says that
    /// inside the arm, the case variable has type `T_i`). This lets fields of
    /// the label's record use the label's offset.
    fn narrowed(&self, name: &str, ty: TypeId) -> TypeId {
        self.narrow
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
            .unwrap_or(ty)
    }

    /// Apply a selector to a place. Select a field or index an array, dereference
    /// a pointer after a `NIL` check, or narrow the type for a guard.
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
                // An open array uses the caller's hidden length as its bound
                // (report section 10.1), not the fixed type's dimension. This
                // makes an index beyond a shorter actual trap.
                if let Some((ptr, len, elem)) = self.open_by_place(&place) {
                    if ix.len() != 1 {
                        return Err(not_yet(
                            &format!("an open array with {} subscripts", ix.len()),
                            span,
                        ));
                    }
                    let i = self.expr(&ix[0])?;
                    let i = if self.b.checks.index {
                        let at = self.bcx.ins().iconst(I32, i64::from(span.lo));
                        self.call_rt(Rt::Index, &[i, len, at])
                            .expect("a bounds check answers with the index")
                    } else {
                        i
                    };
                    let stride = self.b.layout(elem).map(|l| l.size).unwrap_or(0);
                    let wide = self.bcx.ins().sextend(self.b.ptr, i);
                    let off = self.bcx.ins().imul_imm_s(wide, stride as i64);
                    return Ok(Place::Addr(self.bcx.ins().iadd(ptr, off), elem));
                }
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
                let subject_is_ptr =
                    matches!(self.b.types.get(place_ty(&place)), Type::Pointer { .. });
                let target_is_ptr = matches!(self.b.types.get(t), Type::Pointer { .. });
                if subject_is_ptr && !target_is_ptr {
                    // For `p(S)`, read the pointer as a record and use the
                    // pointed-to object as the view, as the C back end does.
                    let (p, _) = self.pointer_value(&place, span)?;
                    return Ok(Place::Addr(p, t));
                }
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

    /// The open array behind a place, if any: its element pointer, hidden length,
    /// and element type. Only the parameter itself has an open type; its fields
    /// and elements have fixed types.
    fn open_by_place(&self, place: &Place) -> Option<(ir::Value, ir::Value, TypeId)> {
        let Place::Addr(ptr, _) = place else {
            return None;
        };
        self.open
            .values()
            .find(|(p, _, _)| p == ptr)
            .copied()
    }

    /// The base address for a `Field` or `Index` selector. For a pointer, load
    /// the object it points to and apply the C back end's `rt_nil(p, at)->f` check;
    /// otherwise use the region's address.
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

    /// A local pointer place already holds its value; an address place must be
    /// loaded to get the pointer value.
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

    /// Read a place from an SSA local or from memory. A region has no value.
    /// `rt_rec_copy` copies it in a statement.
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
                let want = match self.b.value_type(*t) {
                    Some(w) => w,
                    None => {
                        return Err(not_yet(
                            &format!("an assignment to {}", type_name(self.b.types.get(*t))),
                            span,
                        ))
                    }
                };
                // A store's width is the *destination's*, as C's assignment
                // conversion makes it (`w := ADR(x) - ADR(y)` narrows).
                let from = self.value_width(v);
                let v = self.coerce(v, from, want);
                self.bcx.ins().store(ir::MemFlagsData::new(), v, *a, 0);
                Ok(())
            }
        }
    }

    /// The address and type of an array element. This uses row-major order and
    /// checks each index against that dimension's length. The source byte names
    /// a failing access (DESIGN section 9.4). The stride comes from `layout.rs`,
    /// because an element may be a word, pointer, or record.
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

    /// Check `NIL` inside the pointer access, as the C back end does with
    /// `rt_nil(p, at)`. A failure names the designator's source byte.
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

    /// `rt_is(descriptor, &TD_T)` walks the descriptor's `base` chain. A `NIL`
    /// pointer is not a record type (report section 6.4), so return false without
    /// reading through it. The C back end's `IS` faults. Its guard checks first.
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
                // Return false for a `NIL` pointer. Otherwise call the runtime
                // and pass its result to the join block.
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
        // The legal pairings are pointer-to-pointer, pointer-to-the-record it
        // names, and record-to-record (report section 8.1). Check pointer subjects
        // for NIL first, as the C back end does.
        let subject_is_ptr = matches!(self.b.types.get(subject), Type::Pointer { .. });
        let target_is_ptr = matches!(self.b.types.get(target), Type::Pointer { .. });
        let held = if subject_is_ptr {
            true
        } else {
            !target_is_ptr && matches!(self.b.types.get(subject), Type::Record { .. })
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
        let desc = if subject_is_ptr {
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

    /// `NEW`, a copy, an `IS` test, and a guard all take a type descriptor's
    /// address. A pointer has no descriptor of its own; it uses its base record's
    /// descriptor, stored in the object (DESIGN section 9.3).
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

    /// Initialize a region's descriptor and the descriptors of records inside
    /// it (DESIGN section 9.3). The C back end uses `desc_init`. Each view stores
    /// its outermost descriptor at offset 0 because an extension embeds its base
    /// first. Then initialize the base record's fields.
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

    /// The array loop used by `desc_init`, like the C back end's
    /// `for (i__ ...)`. A nested array recurses, so multidimensional record
    /// arrays are initialized the same way.
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

    /// `NEW(p)` calls the runtime allocator and stores the descriptor at the
    /// object's head. Guards and type-`CASE` read it. On allocation failure the
    /// report sets the pointer to `NIL` (section 6.4). This code also writes
    /// descriptors for records inside the object, as the C back end does.
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

    /// Before the module body runs, register every pointer-bearing variable
    /// with the collector and initialize each record variable's descriptor.
    /// This matches the C back end's `__init` preamble (DESIGN section 15.4).
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

    /// `IF ... ELSIF ... ELSE ... END` becomes a chain of blocks, not nested
    /// conditions, so each arm's setup occurs when that arm is reached.
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

    /// Report section 9.6 tries each `ELSIF` guard in order and ends the loop
    /// when none holds. The guards are tested successively.
    fn while_stmt(&mut self, arms: &[(Expr, Vec<Stmt>)]) -> Result<(), Diagnostic> {
        let head = self.bcx.create_block();
        let after = self.bcx.create_block();
        self.jump_to(head);
        // Test each guard in its own block, like the C back end's
        // `if ... else if ... else break`. Each block is entered once; only the
        // loop's back edge returns to `head`.
        let mut test = head;
        for (cond, body) in arms {
            self.bcx.switch_to_block(test);
            let yes = self.bcx.create_block();
            let no = self.bcx.create_block();
            self.branch(cond, yes, no)?;
            self.bcx.switch_to_block(yes);
            self.loops.push(after);
            self.stmts(body)?;
            self.loops.pop();
            self.jump_to(head);
            test = no;
        }
        self.bcx.switch_to_block(test);
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
        // `REPEAT ... UNTIL` evaluates its guard after the body, so the body runs
        // at least once.
        if !self.ended() {
            let yes = self.bcx.create_block();
            self.branch(until, yes, head)?;
            self.bcx.switch_to_block(yes);
        }
        self.jump_to(after);
        self.bcx.switch_to_block(after);
        Ok(())
    }

    /// The bound in `FOR v := b TO e BY k DO S END` is evaluated on each
    /// iteration, as in the report's section 9.8 equivalence and the C back
    /// end's `for` header. `BY` takes a constant expression, so fold it here
    /// and choose the comparison once.
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

    /// A scalar `CASE` uses a chain of comparisons; a record or pointer uses
    /// descriptor `id` values (DESIGN section 9.3), as in the C back end's
    /// `switch (RT_DESC(v)->id)`. If no arm matches, trap (W2.5). Each arm
    /// narrows the case variable to the label's type (report section 9.5).
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
            Type::Basic(Basic::Integer)
            | Type::Basic(Basic::Boolean)
            | Type::Basic(Basic::Char)
            | Type::Basic(Basic::Byte) => {}
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
            let test = self.label_test(value, arm, self.b.unsigned(ty))?;
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
            // The report leaves a value with no matching arm undefined (section
            // 9.5 describes the arms but not that case). This design traps at the
            // source location. `-Ochecks=case` controls the trap; with checks off,
            // execution continues after the arms.
            None if self.b.checks.case_ => self.trap(Msg::CaseDefault, subject.span.lo),
            None => self.jump_to(after),
        }
        self.bcx.switch_to_block(after);
        Ok(())
    }

    /// Whether a scalar subject matches an arm: equality for one label, an
    /// interval for a range, or a combination of those tests for shared labels.
    fn label_test(
        &mut self,
        value: ir::Value,
        arm: &CaseArm,
        unsigned: bool,
    ) -> Result<ir::Value, Diagnostic> {
        // Ranges of unsigned subjects (`CHAR`, `BYTE`) use the report's ordinal
        // order (section 8.2.4).
        let (ge, le) = if unsigned {
            (IntCC::UnsignedGreaterThanOrEqual, IntCC::UnsignedLessThanOrEqual)
        } else {
            (IntCC::SignedGreaterThanOrEqual, IntCC::SignedLessThanOrEqual)
        };
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
                        let a = self.bcx.ins().icmp_imm_s(ge, value, i64::from(lo));
                        let b = self.bcx.ins().icmp_imm_s(le, value, i64::from(hi));
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

    /// A type-`CASE` requires a designator because the arm narrows its name.
    /// Compare the descriptor's `id`; trap when no label matches, using the C
    /// back end's message and source byte. A `NIL` pointer matches no label;
    /// the C back end faults on this case.
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
                // A `NIL` pointer has no descriptor and matches no label. Use an
                // id that cannot match any label, so the report's "no label
                // matches" outcome is a trap rather than a fault.
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
                // A label compares record ids. Reject a type the module has no
                // descriptor for.
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

    /// A statement call can name a module procedure or a predeclared procedure
    /// this slice lowers: `ASSERT` or `NEW`.
    fn call_stmt(&mut self, d: &Designator) -> Result<(), Diagnostic> {
        // Handle qualified SYSTEM procedures first; their names are
        // unambiguous.
        if let Some(entry) = self.system_entry(&d.base).map(str::to_string) {
            let args = self.call_args(d)?.to_vec();
            match entry.as_str() {
                "GET" => return self.sys_get(&args, d.span),
                "PUT" => return self.sys_put(&args, d.span),
                "COPY" => return self.sys_copy(&args, d.span),
                _ => return Err(not_yet(&format!("SYSTEM.{entry}"), d.span)),
            }
        }
        let args = self.call_args(d)?.to_vec();
        // A call to another module uses the symbol and ABI from its interface.
        // The checker records the callee's type at this call site (`call_types`).
        if d.base.len() == 2 {
            let symbol = format!("{}__{}", d.base[0], mangle(&d.base[1]));
            let fref = self.imported_ref(&symbol, d)?;
            let vals = self.pass_args(d, &args)?;
            self.bcx.ins().call(fref, &vals);
            return Ok(());
        }
        let name = self.base_name(d)?;
        let key = self.callee_key(name);
        if let Some(fref) = key.as_ref().and_then(|k| self.frefs.get(k)).copied() {
            let mut vals = self.pass_args(d, &args)?;
            if let Some(link) = self.callee_link(name, d.span)? {
                vals.insert(0, link);
            }
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
                "PACK" | "UNPK" => {
                    // `PACK` and `UNPK` take operand storage (report section
                    // 10.2), so a local scalar uses a frame slot (`address_taken`).
                    // `PACK(x, n)` passes only x's address; `UNPK` writes both
                    // operands.
                    let place = self.actual_place(&args[0])?;
                    let (x, _) = self.address_of(&place, args[0].span)?;
                    let second = match args.get(1) {
                        Some(a) => {
                            if name == "PACK" {
                                self.expr(a)?
                            } else {
                                let place = self.actual_place(a)?;
                                self.address_of(&place, a.span)?.0
                            }
                        }
                        None => return Err(not_yet(&format!("{name} without an exponent"), d.span)),
                    };
                    let fref = self.rt_ref(if name == "PACK" { Rt::Pack } else { Rt::Unpk });
                    self.bcx.ins().call(fref, &[x, second]);
                    return Ok(());
                }
                "INC" | "DEC" | "INCL" | "EXCL" => {
                    let target = args
                        .first()
                        .ok_or_else(|| not_yet(&format!("{name} without an operand"), d.span))?;
                    let place = self.actual_place(target)?;
                    let ty = place_ty(&place);
                    let amount = match args.get(1) {
                        Some(k) => self.expr(k)?,
                        None => self.bcx.ins().iconst(I32, 1),
                    };
                    let step = if self.b.types.is_set(ty) {
                        // `INCL`/`EXCL` add or remove one bit: the mask of the
                        // element `k` is `{k .. k}`, which is the runtime's rule.
                        let fref = self.rt_ref(Rt::Set);
                        let call = self.bcx.ins().call(fref, &[amount, amount]);
                        self.bcx.inst_results(call)[0]
                    } else if self.b.value_type(ty) == Some(types::I8) {
                        self.bcx.ins().ireduce(types::I8, amount)
                    } else {
                        amount
                    };
                    let (current, _) = self.read_place(&place, d.span)?;
                    let next = if name == "DEC" {
                        self.bcx.ins().isub(current, step)
                    } else if name == "EXCL" {
                        let not = self.bcx.ins().bxor_imm_s(step, -1);
                        self.bcx.ins().band(current, not)
                    } else {
                        self.bcx.ins().iadd(current, step)
                    };
                    return self.write_place(&place, next, d.span);
                }
                _ => {}
            }
        }
        Err(not_yet(
            &format!("the call to `{name}` ({} selectors, base {:?})", d.sels.len(), d.base),
            d.span,
        ))
    }

    /// A nested callee expects its parent's frame as the first argument. Pass
    /// this procedure's frame for a child; otherwise walk the links to the
    /// callee's parent (DESIGN section 9.7).
    fn callee_link(&mut self, name: &str, span: Span) -> Result<Option<ir::Value>, Diagnostic> {
        let key = self.callee_key(name).unwrap_or_else(|| name.to_string());
        let callee_level = match self.b.proc_level.get(&key) {
            Some(l) if *l > 1 => *l,
            _ => return Ok(None),
        };
        let link = if callee_level == self.level + 1 {
            self.frame
                .ok_or_else(|| not_yet(&format!("the frame for the call to `{name}`"), span))?
        } else {
            let mut link = self
                .sl
                .ok_or_else(|| not_yet(&format!("the link for the call to `{name}`"), span))?;
            for _ in 0..(self.level.saturating_sub(callee_level)) {
                link = self
                    .bcx
                    .ins()
                    .load(self.b.ptr, ir::MemFlagsData::new(), link, 0);
            }
            link
        };
        Ok(Some(link))
    }

    /// The resolver stores a call's parenthesized arguments in `Selector::Call`
    /// (DESIGN section 5, D2).
    fn call_args<'d>(&self, d: &'d Designator) -> Result<&'d [Expr], Diagnostic> {
        // The last selector is the call; earlier selectors identify the callee
        // (`rc.f(21)` calls the procedure in a record field). A parameterless
        // procedure omits parentheses (report section 9.2), so the resolver
        // creates no `Call` selector.
        match d.sels.last() {
            Some(Selector::Call(args)) => Ok(args),
            None => Ok(&[]),
            other => Err(not_yet(
                &format!("the call `{}` ({other:?})", d.base.join(".")),
                d.span,
            )),
        }
    }

    /// Expand actual arguments to the callee's ABI (DESIGN section 9.7). Pass a
    /// value directly, a `VAR` actual by address, and a structured value as a
    /// caller-side copy's address. Open arrays pass a pointer and length. The
    /// checker records the callee's formals in `call_types`, including for
    /// imported procedures.
    fn pass_args(&mut self, d: &Designator, args: &[Expr]) -> Result<Vec<ir::Value>, Diagnostic> {
        // Use the declaration's formals for a local procedure. For another
        // module, use the callee type recorded here by the checker.
        let local = if d.base.len() == 1 {
            self.callee_key(&d.base[0])
                .and_then(|k| self.b.proc_formals.get(&k).cloned())
        } else {
            None
        };
        let params: Vec<sema::types::Param> = match local {
            Some(formals) => formals
                .iter()
                .map(|f| sema::types::Param {
                    by_var: f.by_var,
                    ty: f.ty,
                })
                .collect(),
            None => {
                // A parameterless call written without parentheses has no type
                // recorded (the checker only checks that it takes none), and
                // there is nothing to pass.
                if args.is_empty() {
                    return Ok(Vec::new());
                }
                // The checker records a call's type at its designator. If it is
                // absent there, use the callee designator without its call
                // selector, as for a record field of procedure type.
                let mut callee_d = d.clone();
                callee_d.sels.pop();
                let pty = self
                    .b
                    .sema
                    .call_types
                    .get(&span_key(d.span))
                    .copied()
                    .or_else(|| self.b.sema.type_at(callee_d.span, self.b.types))
                    .ok_or_else(|| {
                        not_yet(&format!("the call `{}`", d.base.join(".")), d.span)
                    })?;
                match self.b.types.get(pty) {
                    Type::Proc { params, .. } => params.clone(),
                    _ => return Err(not_yet("a call to a non-procedure", d.span)),
                }
            }
        };
        let mut out = Vec::new();
        for (i, arg) in args.iter().enumerate() {
            let Some(pm) = params.get(i) else { break };
            match self.b.types.get(pm.ty) {
                Type::OpenArray { elem } => {
                    let elem = *elem;
                    let (ptr, len) = self.open_actual(arg, elem)?;
                    out.push(ptr);
                    out.push(len);
                }
                _ if pm.by_var => {
                    let place = self.actual_place(arg)?;
                    let (addr, _) = self.address_of(&place, arg.span)?;
                    out.push(addr);
                }
                Type::Record { .. } | Type::Array { .. } => {
                    // The caller copies a structured value argument (report
                    // section 10.1). The callee can write to its copy without
                    // changing the actual. Its size matches the formal type, as
                    // with a C struct assignment.
                    let place = self.actual_place(arg)?;
                    let (src, _) = self.address_of(&place, arg.span)?;
                    let size = self.b.layout(pm.ty).map(|l| l.size).ok_or_else(|| {
                        not_yet("a copy of an argument without a layout", arg.span)
                    })?;
                    let align = self.b.type_align(pm.ty).trailing_zeros() as u8;
                    let slot = self.bcx.create_sized_stack_slot(StackSlotData::new(
                        StackSlotKind::ExplicitSlot,
                        size as u32,
                        align,
                    ));
                    let dst = self.bcx.ins().stack_addr(self.b.ptr, slot, 0);
                    // These are distinct stack and caller locations. Use the
                    // C back end's `memmove` semantics for the struct assignment.
                    self.bcx.emit_small_memory_copy(
                        self.b.obj_config(),
                        dst,
                        src,
                        size,
                        align,
                        align,
                        false,
                        ir::MemFlagsData::new(),
                    );
                    out.push(dst);
                }
                _ => out.push(self.expr(arg)?),
            }
        }
        Ok(out)
    }

    /// A `VAR` or structured actual must be a designator. Pass its storage or a
    /// copy of that storage.
    fn actual_place(&mut self, arg: &Expr) -> Result<Place, Diagnostic> {
        match &arg.kind {
            ExprKind::Designator(d) => self.place(d, arg.span),
            _ => Err(not_yet("this actual parameter, which needs a place", arg.span)),
        }
    }

    /// An open-array actual passes an element pointer and length. A string
    /// literal uses a data object; a fixed array uses its storage and outer
    /// dimension's length. Forward an open-array formal unchanged.
    fn open_actual(&mut self, arg: &Expr, _elem: TypeId) -> Result<(ir::Value, ir::Value), Diagnostic> {
        if let Some(text) = string_constant(self.b.sema, arg) {
            let ptr = self.string_ptr(&text, arg.span)?;
            let len = self.bcx.ins().iconst(I32, text.len() as i64);
            return Ok((ptr, len));
        }
        match &arg.kind {
            ExprKind::Designator(d) => {
                let name = self.base_name(d)?;
                if d.sels.is_empty() {
                    if let Some((ptr, len, _)) = self.open.get(name).copied() {
                        return Ok((ptr, len));
                    }
                }
                let place = self.place(d, arg.span)?;
                let (addr, ty) = self.address_of(&place, arg.span)?;
                let n = self
                    .b
                    .array_dims(ty)
                    .map(|(dims, _)| dims[0])
                    .ok_or_else(|| not_yet("an open array from a non-array", arg.span))?;
                let len = self.bcx.ins().iconst(I32, i64::from(n));
                Ok((addr, len))
            }
            _ => Err(not_yet("this open-array actual", arg.span)),
        }
    }

    /// Build the indirect-call signature from the checker's `call_types` entry
    /// using the ABI of a declared procedure (DESIGN section 9.7).
    fn proc_signature(
        &mut self,
        d: &Designator,
        span: Span,
    ) -> Result<ir::SigRef, Diagnostic> {
        let mut callee_d = d.clone();
        callee_d.sels.pop();
        let pty = self
            .b
            .sema
            .call_types
            .get(&span_key(d.span))
            .copied()
            .or_else(|| self.b.sema.type_at(callee_d.span, self.b.types))
            .ok_or_else(|| not_yet(&format!("the call `{}`", d.base.join(".")), d.span))?;
        let (params, result) = match self.b.types.get(pty) {
            Type::Proc { params, result } => (params.clone(), *result),
            _ => return Err(not_yet("a call to a non-procedure", span)),
        };
        let formals: Vec<Formal> = params
            .iter()
            .enumerate()
            .map(|(i, pm)| Formal {
                name: format!("a{i}"),
                ty: pm.ty,
                by_var: pm.by_var,
            })
            .collect();
        let mut sig = self.b.obj.make_signature();
        sig.params = self.b.abi_params(&formals, span)?;
        if let Some(r) = result {
            sig.returns.push(AbiParam::new(self.b.scalar(r, span)?));
        }
        Ok(self.bcx.import_signature(sig))
    }

    /// Resolve a procedure name by checking the caller's scope, then enclosing
    /// scopes, and finally the module level. A procedure is visible in its
    /// declaring scope and nested scopes, so this walk matches Oberon visibility.
    fn callee_key(&self, name: &str) -> Option<String> {
        let mut path = self.path.clone();
        loop {
            let key = if path.is_empty() {
                name.to_string()
            } else {
                format!("{path}__{name}")
            };
            if self.b.procs.contains_key(&key) {
                return Some(key);
            }
            if path.is_empty() {
                return None;
            }
            path = match path.rsplit_once("__") {
                Some((outer, _)) => outer.to_string(),
                None => String::new(),
            };
        }
    }

    /// The reference to another module's procedure, which the body's walk
    /// declared.
    fn imported_ref(
        &self,
        symbol: &str,
        d: &Designator,
    ) -> Result<ir::FuncRef, Diagnostic> {
        self.imported.get(symbol).copied().ok_or_else(|| {
            not_yet(
                &format!("the call to `{}`", d.base.join(".")),
                d.span,
            )
        })
    }

    /// Return the `SYSTEM.<entry>` named by a designator (DESIGN section 9.10).
    /// The import alias identifies it, so `IMPORT S := SYSTEM` works without
    /// resolving `SYSTEM.sym`.
    fn system_entry<'d>(&self, base: &'d [syntax::ast::Ident]) -> Option<&'d str> {
        if base.len() != 2 {
            return None;
        }
        match alias_of(self.b.module, &base[0]) {
            Some(m) if m == "SYSTEM" => Some(&base[1]),
            _ => None,
        }
    }

    /// Return a `SYSTEM` address expression as `intptr_t`, like the C back end.
    /// A stack address may exceed `INTEGER` (R31), so this expression's IR type
    /// differs from its Oberon type.
    fn address_value(&mut self, e: &Expr) -> Result<ir::Value, Diagnostic> {
        let place = match &e.kind {
            // `SYSTEM.ADR(x)` produces the address itself.
            ExprKind::Designator(d) if matches!(d.sels.last(), Some(Selector::Call(_))) => {
                return self.call_value(d, e.span)
            }
            ExprKind::Designator(d) => self.place(d, e.span)?,
            _ => return Err(not_yet("the address of this expression", e.span)),
        };
        let (addr, _) = self.address_of(&place, e.span)?;
        // The IR pointer type has the target's integer width, so `addr` is
        // already the `intptr_t` value returned by the C back end.
        Ok(addr)
    }

    /// Widen or narrow a value to the width required by its context. The C back
    /// end relies on C conversions, such as comparing an `intptr_t` address with
    /// an `int32_t` length. IR has no implicit conversions. An `ADR` value has
    /// pointer width, not the `INTEGER` width stated by its Oberon type (R31).
    fn value_width(&self, v: ir::Value) -> ir::Type {
        self.bcx.func.dfg.value_type(v)
    }

    /// Convert two values to a common width, choosing the wider type. This
    /// matches C's usual arithmetic conversions, which the emitted C relies on.
    fn same_width(&mut self, a: ir::Value, b: ir::Value) -> (ir::Value, ir::Value) {
        let (ta, tb) = (self.value_width(a), self.value_width(b));
        if ta == tb {
            return (a, b);
        }
        if ta.bits() < tb.bits() {
            (self.coerce(a, ta, tb), b)
        } else {
            (a, self.coerce(b, tb, ta))
        }
    }

    fn coerce(&mut self, v: ir::Value, from: ir::Type, to: ir::Type) -> ir::Value {
        if from == to {
            return v;
        }
        let (fb, tb) = (from.bits(), to.bits());
        if fb < tb {
            if from.is_float() || to.is_float() {
                self.bcx.ins().fpromote(to, v)
            } else {
                self.bcx.ins().sextend(to, v)
            }
        } else if from.is_float() || to.is_float() {
            self.bcx.ins().fdemote(to, v)
        } else {
            self.bcx.ins().ireduce(to, v)
        }
    }

    /// `SYSTEM.SIZE(T)` returns the layout's size in bytes (report section 11.1).
    fn size_of_type(&mut self, e: &Expr, span: Span) -> Result<ir::Value, Diagnostic> {
        let t = match &e.kind {
            ExprKind::Designator(d) => self
                .qident_type(&d.base)
                .or_else(|| self.b.sema.type_at(e.span, self.b.types))
                // Predeclared type names are not declarations (report section
                // 6.1). The report's table uses `SIZE(INTEGER)` as an example.
                .or_else(|| match d.base.as_slice() {
                    [n] => match n.as_str() {
                        "INTEGER" => Some(self.b.types.integer()),
                        "CHAR" => Some(self.b.types.char()),
                        "REAL" => Some(self.b.types.real()),
                        "BOOLEAN" => Some(self.b.types.boolean()),
                        "SET" => Some(self.b.types.set()),
                        "BYTE" => Some(self.b.types.byte()),
                        _ => None,
                    },
                    _ => None,
                }),
            _ => None,
        };
        let t = t.ok_or_else(|| not_yet("SIZE of this argument", span))?;
        let size = self
            .b
            .layout(t)
            .map(|l| l.size)
            .ok_or_else(|| not_yet("SIZE of a type without a layout", span))?;
        Ok(self.bcx.ins().iconst(I32, size as i64))
    }

    /// `SYSTEM.GET(a, v)` reads one word through the byte address and narrows it
    /// to v's type. A `REAL` uses the word's bit pattern directly, not `FLT`'s
    /// conversion.
    fn sys_get(&mut self, args: &[Expr], span: Span) -> Result<(), Diagnostic> {
        let addr = self.address_value(&args[0])?;
        let dst = self.actual_place(&args[1])?;
        let ty = place_ty(&dst);
        let word = self
            .bcx
            .ins()
            .load(I32, ir::MemFlagsData::new(), addr, 0);
        let v = match self.b.types.get(ty) {
            Type::Basic(Basic::Real) => self
                .bcx
                .ins()
                .bitcast(types::F32, ir::MemFlagsData::new(), word),
            Type::Basic(Basic::Char) | Type::Basic(Basic::Byte) => {
                let masked = self.bcx.ins().band_imm_s(word, 0xFF);
                self.bcx.ins().ireduce(types::I8, masked)
            }
            _ => word,
        };
        self.write_place(&dst, v, span)
    }

    /// `SYSTEM.PUT(a, x)` stores x as one word through the byte address. Use x's
    /// type to form the word: `CHAR` contributes its low byte, `REAL` contributes
    /// its bit pattern, and an integer contributes its value.
    fn sys_put(&mut self, args: &[Expr], span: Span) -> Result<(), Diagnostic> {
        let addr = self.address_value(&args[0])?;
        let x = self.expr(&args[1])?;
        let ty = self.type_of(&args[1], span)?;
        let word = match self.b.types.get(ty) {
            Type::Basic(Basic::Real) => self
                .bcx
                .ins()
                .bitcast(I32, ir::MemFlagsData::new(), x),
            Type::Basic(Basic::Char) | Type::Basic(Basic::Byte) => {
                let widened = self.bcx.ins().uextend(I32, x);
                self.bcx.ins().band_imm_s(widened, 0xFF)
            }
            _ => x,
        };
        self.bcx
            .ins()
            .store(ir::MemFlagsData::new(), word, addr, 0);
        Ok(())
    }

    /// `SYSTEM.BIT(a, n)`: bit n of the word the address points at.
    fn sys_bit(&mut self, args: &[Expr], _span: Span) -> Result<ir::Value, Diagnostic> {
        let addr = self.address_value(&args[0])?;
        let n = self.expr(&args[1])?;
        let word = self
            .bcx
            .ins()
            .load(I32, ir::MemFlagsData::new(), addr, 0);
        let shifted = self.bcx.ins().ushr(word, n);
        Ok(self.bcx.ins().band_imm_s(shifted, 1))
    }

    /// Report section 11.1 defines `mem[a]` as an array of words. This address is
    /// byte-based.
    fn sys_copy(&mut self, args: &[Expr], _span: Span) -> Result<(), Diagnostic> {
        // Report section 11.1 copies from x to y. The C back end's
        // `memcpy(dst, src, ...)` uses the same order.
        let src = self.address_value(&args[0])?;
        let dst = self.address_value(&args[1])?;
        let n = self.expr(&args[2])?;
        let words = self.bcx.ins().iconst(I32, 4);
        let bytes = self.bcx.ins().imul(n, words);
        let wide = self.bcx.ins().sextend(self.b.ptr, bytes);
        let cfg = self.b.obj_config();
        self.bcx.call_memcpy(cfg, dst, src, wide);
        Ok(())
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
        // Check SYSTEM functions first. The import alias identifies the entry
        // (DESIGN section 9.10).
        if let Some(entry) = self.system_entry(&d.base).map(str::to_string) {
            let args = self.call_args(d)?.to_vec();
            return match entry.as_str() {
                "ADR" => self.address_value(&args[0]),
                "SIZE" => self.size_of_type(&args[0], span),
                "BIT" => self.sys_bit(&args, span),
                _ => Err(not_yet(&format!("SYSTEM.{entry}"), span)),
            };
        }
        // The checker does not record a user's own `MIN`; shadowing needs no
        // check. The C back end reads the same table.
        if let Some(v) = self.b.sema.extreme_at(span) {
            return Ok(self.bcx.ins().iconst(I32, i64::from(v)));
        }
        // An expression call to another module uses the same symbol and ABI as a
        // statement call (`Lib__Twice`). `imported_ref` gets the signature,
        // including the result type, from the checker's `call_types` entry.
        if d.base.len() == 2 {
            let symbol = format!("{}__{}", d.base[0], mangle(&d.base[1]));
            let fref = self.imported_ref(&symbol, d)?;
            let args = self.call_args(d)?.to_vec();
            let vals = self.pass_args(d, &args)?;
            let call = self.bcx.ins().call(fref, &vals);
            return self.bcx.inst_results(call).first().copied().ok_or_else(|| {
                not_yet(&format!("the call to `{symbol}` as a value"), d.span)
            });
        }
        let name = self.base_name(d)?.to_string();
        let key = self.callee_key(&name);
        if let Some(fref) = key.as_ref().and_then(|k| self.frefs.get(k)).copied() {
            let args = self.call_args(d)?.to_vec();
            let mut vals = self.pass_args(d, &args)?;
            if let Some(link) = self.callee_link(&name, d.span)? {
                vals.insert(0, link);
            }
            let call = self.bcx.ins().call(fref, &vals);
            return self
                .bcx
                .inst_results(call)
                .first()
                .copied()
                .ok_or_else(|| not_yet(&format!("the call to `{name}` as a value"), span));
        }
        // Use a predeclared function only if the program has not declared that
        // name (report section 4's visibility rule).
        if !self.vars.contains_key(&name) && !self.shadows(&name) {
            let args = self.call_args(d)?.to_vec();
            match name.as_str() {
                // `ABS` reads its operand once. Use a conditional in IR; the C
                // path uses a helper for the same reason.
                "ABS" => {
                    let x = self.expr(&args[0])?;
                    if self.b.is_real(self.type_of(&args[0], span)?) {
                        return Ok(self.bcx.ins().fabs(x));
                    }
                    let neg = self.bcx.ins().ineg(x);
                    let below = self.bcx.ins().icmp_imm_s(IntCC::SignedLessThan, x, 0);
                    return Ok(self.bcx.ins().select(below, neg, x));
                }
                "ODD" => {
                    let x = self.expr(&args[0])?;
                    return Ok(self.bcx.ins().band_imm_s(x, 1));
                }
                "LSL" | "ASR" | "ROR" => {
                    let x = self.expr(&args[0])?;
                    let n = self.expr(&args[1])?;
                    return self.shift(&name, x, n);
                }
                "FLOOR" => {
                    let x = self.expr(&args[0])?;
                    let floor = self.bcx.ins().floor(x);
                    return Ok(self.bcx.ins().fcvt_to_sint_sat(I32, floor));
                }
                "FLT" => {
                    let x = self.expr(&args[0])?;
                    return Ok(self.bcx.ins().fcvt_from_sint(types::F32, x));
                }
                "ORD" => {
                    let t = self.type_of(&args[0], span)?;
                    if let ExprKind::Str(text) = &args[0].kind {
                        let first = text.as_bytes().first().copied().unwrap_or(0);
                        return Ok(self.bcx.ins().iconst(I32, i64::from(first)));
                    }
                    let x = self.expr(&args[0])?;
                    if self.b.value_type(t) == Some(types::I8) {
                        return Ok(self.bcx.ins().uextend(I32, x));
                    }
                    return Ok(x);
                }
                "CHR" => {
                    let x = self.expr(&args[0])?;
                    return Ok(self.bcx.ins().ireduce(types::I8, x));
                }
                "LEN" => {
                    let n = match &args[0].kind {
                        ExprKind::Designator(d) => d.base[0].clone(),
                        _ => return Err(not_yet("LEN of this expression", span)),
                    };
                    match self.open.get(&n) {
                        Some((_, len, _)) => return Ok(*len),
                        None => return Err(not_yet("LEN of something that is not an array", span)),
                    }
                }
                _ => {}
            }
        }
        // A call through a procedure *value*: a variable or a parameter. A
        // procedure-valued *field* (`rc.f(21)`) is the same shape one step on and
        // is refused rather than lowered wrongly: it is the last case
        // `Regress.mod` needs, and the tree says so by name.
        let mut callee_d = d.clone();
        callee_d.sels.pop();
        let place = self.place(&callee_d, span)?;
        let (callee, _) = self.pointer_value(&place, span)?;
        let sigref = self.proc_signature(d, span)?;
        let args = self.call_args(d)?.to_vec();
        let vals = self.pass_args(d, &args)?;
        let call = self.bcx.ins().call_indirect(sigref, callee, &vals);
        self.bcx
            .inst_results(call)
            .first()
            .copied()
            .ok_or_else(|| not_yet(&format!("the call to `{name}` as a value"), span))
    }

    /// The report's shift rules (section 8.2.2) differ from the IR (DESIGN
    /// section 16). `LSL(x,n)` is `x * 2^n`, and `ASR(x,n)` is `x DIV 2^n`.
    /// Counts of 32 or more produce 0 for `LSL` and 0 or -1 for `ASR`.
    /// Cranelift masks counts to five bits, as did the C runtime before R22.
    /// Mask the count here and use the sign for large `ASR` shifts. The corpus
    /// tests 32, 33, and 63.
    fn shift(&mut self, name: &str, x: ir::Value, n: ir::Value) -> Result<ir::Value, Diagnostic> {
        let k = self.bcx.ins().band_imm_s(n, 31);
        match name {
            // The report defines `ROR` modulo 32. Shift by complementary counts
            // and combine the results. `ROR` masks the input count.
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


    /// Convert a boolean expression to the `int32_t` value C's `&&`/`||` return.
    /// Here `BOOLEAN` is an `int32_t` holding 0 or 1 (DESIGN section 9.2).
    fn bool_value(&mut self, e: &Expr) -> Result<ir::Value, Diagnostic> {
        match &e.kind {
            // `&` and `OR` evaluate the right operand only if the left does not
            // determine the result. C's `&&`/`||` and e2e tests require this.
            // Build two blocks and merge the answer as the join's argument.
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
            ExprKind::Binary(op, l, r) if is_relation(*op) => self.relation(*op, l, r, e.span),
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

    /// A relation returns the `i8` branch condition. Signedness follows the
    /// operand type. Read `CHAR` as unsigned, matching C's `(unsigned char)`
    /// casts and the report's ordinal order (section 8.2.4). Compare `REAL`
    /// values with `fcmp`, which returns false for NaN as C does. Compare
    /// character arrays with the runtime's length-bounded `rt_charcmp`.
    fn relation(
        &mut self,
        op: BinOp,
        l: &Expr,
        r: &Expr,
        span: Span,
    ) -> Result<ir::Value, Diagnostic> {
        let lt = self.type_of(l, span)?;
        if self.b.types.is_char_array(lt) {
            let (a, na) = self.char_array_operand(l)?;
            let (b, nb) = self.char_array_operand(r)?;
            let fref = self.rt_ref(Rt::CharCmp);
            let call = self.bcx.ins().call(fref, &[a, na, b, nb]);
            let cmp = self.bcx.inst_results(call)[0];
            let zero = self.bcx.ins().iconst(I32, 0);
            return Ok(self.bcx.ins().icmp(relation_cc(op), cmp, zero));
        }
        let a = self.expr(l)?;
        let b = self.expr(r)?;
        let value_type = self.b.value_type(lt).unwrap_or(I32);
        if value_type == types::F32 {
            return Ok(self.bcx.ins().fcmp(float_cc(op), a, b));
        }
        let (a, b) = self.same_width(a, b);
        let cc = if self.b.unsigned(lt) {
            unsigned_cc(op)
        } else {
            relation_cc(op)
        };
        Ok(self.bcx.ins().icmp(cc, a, b))
    }

    /// The address of a character array and its length, for the bounded
    /// comparison: fixed arrays use their storage, and string literals use data
    /// objects with the same bytes as the C back end's literals.
    fn char_array_operand(&mut self, e: &Expr) -> Result<(ir::Value, ir::Value), Diagnostic> {
        if let Some(text) = string_constant(self.b.sema, e) {
            let ptr = self.string_ptr(&text, e.span)?;
            let n = self.bcx.ins().iconst(I32, text.len() as i64 + 1);
            return Ok((ptr, n));
        }
        match &e.kind {
            ExprKind::Designator(d) => {
                let place = self.place(d, e.span)?;
                let (addr, ty) = self.address_of(&place, e.span)?;
                let n = self
                    .b
                    .types
                    .array_len(ty)
                    .map(|n| self.bcx.ins().iconst(I32, i64::from(n)))
                    .unwrap_or_else(|| self.bcx.ins().iconst(I32, 0));
                Ok((addr, n))
            }
            _ => Err(not_yet("a character array in a comparison", e.span)),
        }
    }

    /// Return the address of a string literal's bytes in a data object, like the
    /// C back end's literal in `.rodata`.
    fn string_ptr(&mut self, text: &str, span: Span) -> Result<ir::Value, Diagnostic> {
        let gv = *self.strings.get(&span.lo).ok_or_else(|| {
            not_yet(
                &format!("the string {text:?} where a pointer is expected"),
                span,
            )
        })?;
        Ok(self.bcx.ins().symbol_value(self.b.ptr, gv))
    }

    /// Branch on a condition. `&` and `OR` become control flow, so the right
    /// operand is skipped when the left operand determines the result.
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
        if matches!(&e.kind, ExprKind::Designator(d) if d.sels.is_empty()) {
            if let Some(value) = self.b.sema.const_at(e.span) {
                return Ok(match value {
                    const_eval::Const::Int(n) => self.bcx.ins().iconst(I32, i64::from(n)),
                    const_eval::Const::Real(r) => self.bcx.ins().f32const(r),
                    const_eval::Const::Bool(b) => self.bcx.ins().iconst(I32, i64::from(b)),
                    const_eval::Const::Char(c) => self.bcx.ins().iconst(types::I8, i64::from(c)),
                    const_eval::Const::Str(text) if text.len() <= 1 => {
                        let first = text.as_bytes().first().copied().unwrap_or(0);
                        self.bcx.ins().iconst(types::I8, i64::from(first))
                    }
                    const_eval::Const::Str(_) => {
                        return Err(not_yet("a string used as a value", e.span));
                    }
                    const_eval::Const::Set(bits) => self.bcx.ins().iconst(I32, i64::from(bits)),
                });
            }
        }
        match &e.kind {
            // `2147483648` represents `MIN(INTEGER)`, so the literal uses `u32`
            // (DESIGN section 4).
            ExprKind::Int(v) => Ok(self.bcx.ins().iconst(I32, i64::from(*v as i32))),
            ExprKind::Bool(b) => Ok(self.bcx.ins().iconst(I32, i64::from(*b))),
            ExprKind::Nil => Ok(self.bcx.ins().iconst(self.b.ptr, 0)),
            // The C back end writes `((float)3.5f)`, so a literal is narrowed to
            // the back end's REAL here too.
            ExprKind::Real(v) => Ok(self.bcx.ins().f32const(*v as f32)),
            ExprKind::Char(c) => Ok(self.bcx.ins().iconst(types::I8, i64::from(*c))),
            // A one-character string used as a value is read as `CHAR` (report
            // section 10.1, with examples `ch <= "9"` and `ORD("0")`). This
            // matches C's `"x"[0]`. Longer strings are arrays, not values.
            ExprKind::Str(text) => {
                let first = text.as_bytes().first().copied().unwrap_or(0);
                if text.len() <= 1 {
                    Ok(self.bcx.ins().iconst(types::I8, i64::from(first)))
                } else {
                    Err(not_yet("a string used as a value", e.span))
                }
            }
            // `{a .. b}` calls runtime `rt_set` once per element, then ORs the
            // masks. The runtime handles clamping and width 32.
            // runtime's.
            ExprKind::Set(elems) => {
                let fref = self.rt_ref(Rt::Set);
                let mut bits: Option<ir::Value> = None;
                for el in elems {
                    let lo = self.expr(&el.lo)?;
                    let hi = match &el.hi {
                        Some(h) => self.expr(h)?,
                        None => lo,
                    };
                    let call = self.bcx.ins().call(fref, &[lo, hi]);
                    let one = self.bcx.inst_results(call)[0];
                    bits = Some(match bits {
                        None => one,
                        Some(b) => self.bcx.ins().bor(b, one),
                    });
                }
                // `{}` is the empty set, and a SET literal with no elements has
                // no calls at all.
                Ok(bits.unwrap_or_else(|| self.bcx.ins().iconst(I32, 0)))
            }
            ExprKind::Designator(d) => self.designator_value(d, e.span),
            ExprKind::TypeTest(x, q) => {
                let target = self
                    .qident_type(q)
                    .ok_or_else(|| not_yet("the type of an IS test", e.span))?;
                self.is_test(x, target, e.span)
            }
            ExprKind::Unary(op, x) => {
                let v = self.expr(x)?;
                let ty = self.type_of(x, e.span)?;
                Ok(match op {
                    UnOp::Plus => v,
                    // `-` negates REAL values and complements SET values (report
                    // section 8.2.3).
                    UnOp::Neg if self.b.is_real(ty) => self.bcx.ins().fneg(v),
                    UnOp::Neg if self.b.types.is_set(ty) => self.bcx.ins().bxor_imm_s(v, -1),
                    UnOp::Neg => self.bcx.ins().ineg(v),
                    // `~` negates a BOOLEAN (report section 8.2.1). A BOOLEAN
                    // uses only one bit.
                    UnOp::Not => self.bcx.ins().bxor_imm_s(v, 1),
                })
            }
            ExprKind::Binary(op, l, r) => self.binary_value(*op, l, r, e),
        }
    }

    fn binary_value(
        &mut self,
        op: BinOp,
        l: &Expr,
        r: &Expr,
        whole: &Expr,
    ) -> Result<ir::Value, Diagnostic> {
        // Comparisons and `&`/`OR` produce BOOLEAN values. `bool_value` also
        // preserves short-circuit evaluation.
        if is_relation(op) || matches!(op, BinOp::And | BinOp::Or) {
            return self.bool_value(whole);
        }
        let lt = self.type_of(l, whole.span)?;
        let a = self.expr(l)?;
        let b = self.expr(r)?;
        // REAL operations use float instructions. The checker rejects mixed
        // INTEGER/REAL operands in the C back end.
        if self.b.is_real(lt) {
            return Ok(match op {
                BinOp::Add => self.bcx.ins().fadd(a, b),
                BinOp::Sub => self.bcx.ins().fsub(a, b),
                BinOp::Mul => self.bcx.ins().fmul(a, b),
                BinOp::Div => self.bcx.ins().fdiv(a, b),
                other => {
                    return Err(not_yet(
                        &format!("the operator {other:?} on REAL"),
                        whole.span,
                    ))
                }
            });
        }
        // For SET values, `+`, `-`, `*`, and `/` mean union, difference,
        // intersection, and symmetric difference (report section 8.2.3).
        if self.b.types.is_set(lt) {
            return Ok(match op {
                BinOp::Add => self.bcx.ins().bor(a, b),
                BinOp::Sub => {
                    let not = self.bcx.ins().bxor_imm_s(b, -1);
                    self.bcx.ins().band(a, not)
                }
                BinOp::Mul => self.bcx.ins().band(a, b),
                BinOp::Div => self.bcx.ins().bxor(a, b),
                other => {
                    return Err(not_yet(
                        &format!("the operator {other:?} on SET"),
                        whole.span,
                    ))
                }
            });
        }
        // `x IN s` tests bit `x & 31` in the word (report section 9.5).
        if op == BinOp::In {
            let shift = self.bcx.ins().band_imm_s(a, 31);
            let word = self.bcx.ins().ushr(b, shift);
            // The mask test returns a 0/1 word, which the C back end narrows
            // with `(int32_t)(...)`.
            return Ok(self.bcx.ins().band_imm_s(word, 1));
        }
        // An address difference is pointer-wide. Its other operand may be an
        // `INTEGER`, so convert both values to one width.
        let (a, b) = self.same_width(a, b);
        Ok(match op {
            BinOp::Add => self.bcx.ins().iadd(a, b),
            BinOp::Sub => self.bcx.ins().isub(a, b),
            // IR `iadd`, `isub`, and `imul` wrap. This matches `-fwrapv` in the C
            // path (DESIGN section 9.1, R4).
            BinOp::Mul => self.bcx.ins().imul(a, b),
            // `DIV` and integer `/` use floor division with a divisor check
            // (report section 8.2.2). The constant folder treats `/` as `DIV`,
            // so both paths must agree. The runtime supplies the message and
            // the left operand's source byte, as in the emitted C.
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
        // A constant designator uses the value recorded by the checker. The C
        // path folds it too (R20, PLAN.md W6.2).
        if let Some(c) = self.b.sema.const_at(span) {
            return match c.int() {
                Some(v) => Ok(self.bcx.ins().iconst(I32, i64::from(v))),
                None => Err(not_yet("a non-INTEGER constant", span)),
            };
        }
        // A declared procedure used as a value is its address. This allows
        // assignment to a procedure variable (report section 6.5).
        if d.sels.is_empty() {
            if let Some(fref) = self.frefs.get(&d.base[0]).copied() {
                return Ok(self.bcx.ins().func_addr(self.b.ptr, fref));
            }
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

    /// A guard or type-`CASE` label names a type declared in this module. Report
    /// section 9.5 restricts labels to the module's own types.
    fn qident_type(&self, q: &syntax::ast::QIdent) -> Option<TypeId> {
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

    /// Emit a trap that the runtime reports, then end the path. The signature
    /// has no `noreturn` bit (DESIGN section 16), so emit an IR trap as well.
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

    /// Check whether the current block already ends in an instruction that
    /// stops control flow. Cranelift's `is_unreachable` instead checks for a
    /// sealed block with no predecessors. A `RETURN` inside an arm stops it.
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

/// Integer relations are signed (DESIGN section 9.2); BOOLEAN equality works
/// either way. Compare `CHAR` and `BYTE` by ordinal (report section 8.2.4), and
/// compare SET values as `uint32_t` masks.
fn unsigned_cc(op: BinOp) -> IntCC {
    match op {
        BinOp::Eq => IntCC::Equal,
        BinOp::Ne => IntCC::NotEqual,
        BinOp::Lt => IntCC::UnsignedLessThan,
        BinOp::Le => IntCC::UnsignedLessThanOrEqual,
        BinOp::Gt => IntCC::UnsignedGreaterThan,
        _ => IntCC::UnsignedGreaterThanOrEqual,
    }
}

/// The float pair, for a REAL comparison: `fcmp` says false for a NaN, which is
/// what C's `<` on floats says too.
fn float_cc(op: BinOp) -> ir::condcodes::FloatCC {
    use ir::condcodes::FloatCC as F;
    match op {
        BinOp::Eq => F::Equal,
        BinOp::Ne => F::NotEqual,
        BinOp::Lt => F::LessThan,
        BinOp::Le => F::LessThanOrEqual,
        BinOp::Gt => F::GreaterThan,
        _ => F::GreaterThanOrEqual,
    }
}

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
        // A string label is a `CHAR` label written as a one-character string
        // (report section 9.5's appendix). Use the first byte, as the C back end
        // does.
        CaseLabel::Str(s) => Ok(i32::from(s.as_bytes().first().copied().unwrap_or(0))),
        CaseLabel::Named(_) => Err(not_yet("a type CASE label", span)),
    }
}

