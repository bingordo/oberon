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
}

impl Msg {
    fn text(self) -> &'static str {
        match self {
            Msg::Assertion => "assertion failed",
            Msg::CaseDefault => "CASE: no matching label",
        }
    }

    /// The data object's name. Local, like every generated name: a C string
    /// literal has no symbol at all, so no client can depend on this spelling.
    fn symbol(self) -> &'static str {
        match self {
            Msg::Assertion => "assertion_failed",
            Msg::CaseDefault => "case_no_matching_label",
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
}

impl Rt {
    fn symbol(self) -> &'static str {
        match self {
            Rt::Index => "ob_rt_index_at",
            Rt::Div => "ob_rt_div_at",
            Rt::Mod => "ob_rt_mod_at",
            Rt::Trap => "ob_rt_trap_pos",
        }
    }
}

/// Lower one checked module to an object file, or say which construct it cannot
/// lower yet — as a diagnostic with that construct's span, so the driver prints
/// it like any other failure.
pub fn emit_object(
    module: &Module,
    types: &Types,
    sema: &Sema<'_>,
    checks: cgen::Checks,
) -> Result<Vec<u8>, Vec<Diagnostic>> {
    build(module, types, sema, checks).map_err(|d| vec![d])
}

fn build(
    module: &Module,
    types: &Types,
    sema: &Sema<'_>,
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
        module: &Module,
        types: &'a Types,
        sema: &'a Sema<'a>,
        checks: cgen::Checks,
        obj: ObjectModule,
    ) -> Backend<'a> {
        let ptr = obj.target_config().pointer_type();
        let ptr_bytes = u64::from(obj.target_config().pointer_bytes());
        Backend {
            module_name: module.name.clone(),
            types,
            sema,
            checks,
            obj,
            ptr,
            ptr_bytes,
            layouts: HashMap::new(),
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
        for msg in [Msg::Assertion, Msg::CaseDefault] {
            let id = self.declare_string(msg.symbol(), msg.text())?;
            self.msgs.insert(msg, id);
        }
        // 2. The runtime entry points, and the registry. A declaration is a
        //    symbol-table entry and not code — the linker tolerates one that is
        //    never called, which the C path's mixture of modules already relies
        //    on — so they are declared once for the module; what the code
        //    *calls* is decided where it is lowered, and that is what reaches
        //    the object.
        for rt in [Rt::Index, Rt::Div, Rt::Mod, Rt::Trap] {
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
        self.layouts = layouts.resolved();
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

    /// The name a module-level Oberon object is emitted under. The same rule as
    /// the C back end's, C-keyword escape included, because a client compiled
    /// against the module's `.h` looks for exactly this symbol.
    fn symbol(&self, name: &str) -> String {
        format!("{}__{}", self.module_name, mangle(name))
    }

    fn rt_signature(&self, rt: Rt) -> Signature {
        let mut sig = self.obj.make_signature();
        match rt {
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

    /// The one type decision the fragment makes. `INTEGER` and `BOOLEAN` are the
    /// IR's `i32`; everything else is refused *here*, where the program's own
    /// naming can be used, rather than somewhere in the lowering where it would
    /// read as an internal failure.
    fn scalar(&self, t: TypeId, span: Span) -> Result<ir::Type, Diagnostic> {
        match self.types.get(t) {
            Type::Basic(Basic::Integer) | Type::Basic(Basic::Boolean) => Ok(I32),
            other => Err(not_yet(
                &format!("a value of type {}", type_name(other)),
                span,
            )),
        }
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
                data.set_align(WORD);
                self.obj
                    .define_data(id, &data)
                    .map_err(|e| host(format!("{e}")))?;
                self.vars.insert(name.clone(), id);
                self.var_ty.insert(name.clone(), ty);
            }
        }
        Ok(())
    }

    /// What a variable of this type holds, from the layout table, or a refusal.
    /// A fixed array of the scalar types is what this back end stores today —
    /// the corpus's self-checks need `fail: ARRAY 1 OF INTEGER`, and a
    /// multi-dimensional one is the same arithmetic on the same layout — while
    /// an array of records or pointers needs the elements themselves lowered
    /// (PLAN.md W9.2b).
    fn data_size(&self, t: TypeId, span: Span) -> Result<usize, Diagnostic> {
        match self.array_dims(t) {
            // Any number of dimensions: what has to be lowerable is the
            // *innermost* element, not the immediate one (the immediate one of a
            // `ARRAY 3, 4 OF INTEGER` is an `ARRAY 4 OF INTEGER`).
            Some((_, elem)) => {
                self.scalar(elem, span)?;
            }
            None => {
                self.scalar(t, span)?;
            }
        }
        match self.layout(t) {
            Some(l) => Ok(l.size as usize),
            None => Err(not_yet(
                &format!("a variable of type {}", type_name(self.types.get(t))),
                span,
            )),
        }
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

        let mut fn_ctx = FunctionBuilderContext::new();
        {
            let mut bcx = FunctionBuilder::new(&mut ctx.func, &mut fn_ctx);
            let entry = bcx.create_block();
            bcx.append_block_params_for_function_params(entry);
            bcx.switch_to_block(entry);
            let mut vars = HashMap::new();
            let values: Vec<ir::Value> = bcx.block_params(entry).to_vec();
            for (i, (pname, _)) in params.iter().enumerate() {
                let var = bcx.declare_var(I32);
                bcx.def_var(var, values[i]);
                vars.insert(pname.clone(), var);
            }
            let mut slots: HashMap<String, ir::Value> = HashMap::new();
            let mut slot_ty: HashMap<String, TypeId> = HashMap::new();
            for (lname, ty) in locals {
                if matches!(self.types.get(*ty), Type::Array { .. }) {
                    let size = self.layout(*ty).map(|l| l.size).ok_or_else(|| {
                        not_yet(&format!("the local array `{lname}`"), span)
                    })?;
                    if size > u64::from(u32::MAX) {
                        // A frame slot's size is a 32-bit field in Cranelift's IR;
                        // the C path's local is a `sizeof`-sized object.
                        return Err(Diagnostic::error(
                            span,
                            format!(
                                "the local array `{lname}` is {size} bytes; the clif back end's                                  stack slots are 32-bit"
                            ),
                        ));
                    }
                    let align = self
                        .types
                        .elem(*ty)
                        .and_then(|e| self.layout(e))
                        .map(|l| l.align)
                        .unwrap_or(1)
                        .trailing_zeros() as u8;
                    let slot = bcx.create_sized_stack_slot(StackSlotData::new(
                        StackSlotKind::ExplicitSlot,
                        size as u32,
                        align,
                    ));
                    let addr = bcx.ins().stack_addr(self.ptr, slot, 0);
                    // Zeroed exactly as the C back end zeroes a local
                    // (`memset`), so a program that reads an element before
                    // assigning it reads the same value on both paths — which is
                    // the one value the report leaves free.
                    bcx.emit_small_memset(
                        self.obj.target_config(),
                        addr,
                        0,
                        size,
                        align,
                        ir::MemFlagsData::new(),
                    );
                    slots.insert(lname.clone(), addr);
                    slot_ty.insert(lname.clone(), *ty);
                } else {
                    let var = bcx.declare_var(self.scalar(*ty, span)?);
                    vars.insert(lname.clone(), var);
                }
            }
            let mut l = Lower::new(self, bcx, vars, slots, slot_ty, gvs, frefs, rts, msgs, result);
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

/// The IR for one procedure: SSA values for its locals, a data object for each
/// module variable, and the references its body may name.
struct Lower<'b, 'f, 'a> {
    b: &'b Backend<'a>,
    bcx: FunctionBuilder<'f>,
    vars: HashMap<String, Variable>,
    /// A local whose type is an array: its storage is a zeroed stack slot, and
    /// what the body names is the address (cgen zeroes a local the same way).
    slots: HashMap<String, ir::Value>,
    /// The type of each of those slots, which is where the stride comes from.
    /// A module variable's array type is looked up in the back end's table
    /// instead, since its storage is a data object rather than a frame.
    slot_ty: HashMap<String, TypeId>,
    gv: HashMap<String, ir::GlobalValue>,
    frefs: HashMap<String, ir::FuncRef>,
    rt: HashMap<Rt, ir::FuncRef>,
    msgs: HashMap<Msg, ir::GlobalValue>,
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
        slot_ty: HashMap<String, TypeId>,
        gv: HashMap<String, ir::GlobalValue>,
        frefs: HashMap<String, ir::FuncRef>,
        rt: HashMap<Rt, ir::FuncRef>,
        msgs: HashMap<Msg, ir::GlobalValue>,
        result: Option<TypeId>,
    ) -> Lower<'b, 'f, 'a> {
        Lower {
            b,
            bcx,
            vars,
            slots,
            slot_ty,
            gv,
            frefs,
            rt,
            msgs,
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
        let v = self.expr(value)?;
        let name = self.base_name(target)?;
        match target.sels.as_slice() {
            [] => self.store_var(name, v, target.span),
            // One or more subscripts, each checked against its own dimension.
            [Selector::Index(ix)] => {
                let (addr, _) = self.elem_addr(name, ix, target.span)?;
                self.bcx.ins().store(ir::MemFlagsData::new(), v, addr, 0);
                Ok(())
            }
            _ => Err(not_yet(
                &format!("the assignment target `{name}` with its selectors"),
                target.span,
            )),
        }
    }

    /// The single name a designator starts with. A qualified one (`M.x`) needs
    /// the other module's symbol, which is what importing a *lowered* module
    /// will need (PLAN.md W9.2).
    fn base_name<'d>(&self, d: &'d Designator) -> Result<&'d str, Diagnostic> {
        match d.base.as_slice() {
            [name] => Ok(name.as_str()),
            _ => Err(not_yet(
                &format!("the qualified designator `{}`", d.base.join(".")),
                d.span,
            )),
        }
    }

    /// The address of one element of an array, and the element's IR type, with
    /// the bounds checks the C back end makes: `rt_index(i, n, at)` answers with
    /// the index and traps with the *designator's* source byte when it is out of
    /// range, which is what makes a failing self-check name its line (DESIGN
    /// §9.4). Row-major, one check per dimension and the *same* byte for each of
    /// them, exactly as the C emitter's chain of `rt_index` calls is.
    ///
    /// The stride comes from `layout.rs` rather than from a literal: the element
    /// may be a word, a `CHAR`, or (once their elements are lowered, PLAN.md
    /// W9.2b) a record, and the layout is the one the C side allocates by.
    fn elem_addr(
        &mut self,
        name: &str,
        indexes: &[Expr],
        span: Span,
    ) -> Result<(ir::Value, ir::Type), Diagnostic> {
        let ty = match self
            .slot_ty
            .get(name)
            .or_else(|| self.b.var_ty.get(name))
            .copied()
        {
            Some(t) => t,
            None => return Err(not_yet(&format!("the array `{name}`"), span)),
        };
        let (dims, elem) = self
            .b
            .array_dims(ty)
            .ok_or_else(|| not_yet(&format!("an index into `{name}`"), span))?;
        if indexes.len() != dims.len() {
            return Err(not_yet(
                &format!("an index into `{name}` with {} subscripts", indexes.len()),
                span,
            ));
        }
        // The base: a module variable is a data object, a local an address in
        // its own frame.
        let base = if let Some(addr) = self.slots.get(name).copied() {
            addr
        } else if let Some(gv) = self.gv.get(name).copied() {
            self.bcx.ins().symbol_value(self.b.ptr, gv)
        } else {
            return Err(not_yet(&format!("the array `{name}`"), span));
        };
        let elem_ty = self.b.scalar(elem, span)?;
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
            // `flat = flat * dims[k] + i`, which is the row-major address the C
            // emitter's nested `.e[...]` chain computes.
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
        Ok((self.bcx.ins().iadd(base, off), elem_ty))
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
        self.store_var(var, start, span)?;
        let head = self.bcx.create_block();
        let body_b = self.bcx.create_block();
        let after = self.bcx.create_block();
        self.jump_to(head);
        self.bcx.switch_to_block(head);
        let v = self.load_var(var, span)?;
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
            let v = self.load_var(var, span)?;
            let next = self.bcx.ins().iadd_imm_s(v, i64::from(step));
            self.store_var(var, next, span)?;
            self.jump_to(head);
        }
        self.bcx.switch_to_block(after);
        Ok(())
    }

    /// A scalar `CASE`: a chain of comparisons, one per label, then the trap the
    /// C back end's `default:` makes when no arm matches. A type-`CASE` switches
    /// on a descriptor id and needs records (PLAN.md W9.2), and the subject's
    /// type is what tells the two apart.
    fn case_stmt(
        &mut self,
        subject: &Expr,
        arms: &[CaseArm],
        span: Span,
    ) -> Result<(), Diagnostic> {
        let ty = self
            .b
            .sema
            .type_at(subject.span, self.b.types)
            .unwrap_or(TypeId(0));
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
            // No default arm: a value that matches no label traps, as it does
            // in the type-`CASE` (§9.3) — W2.5's decision, and what the C back
            // end now emits (`default: rt_trap_pos("CASE: no matching label")`).
            // The report leaves the case undefined (§9.5 defines which arm runs
            // and says nothing about a value that matches no label), and this
            // design's answer to undefined is a located trap. `-Ochecks=case`
            // still governs it; with the check off, the run falls out of the
            // arms as it always did.
            None if self.b.checks.case_ => self.trap(Msg::CaseDefault, subject.span.lo),
            None => self.jump_to(after),
        }
        self.bcx.switch_to_block(after);
        Ok(())
    }

    /// Whether the subject is one of an arm's labels: an equality for a single
    /// label, an interval for a range, and a disjunction of those for the labels
    /// that share the arm. A range is two comparisons rather than one per label,
    /// which is the part the C path leaves to the C compiler's jump table.
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
                        let b = self.bcx.ins().icmp_imm_s(
                            IntCC::SignedLessThanOrEqual,
                            value,
                            i64::from(hi),
                        );
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

    /// A call used as a statement: a procedure of this module, or `ASSERT`.
    fn call_stmt(&mut self, d: &Designator) -> Result<(), Diagnostic> {
        let name = self.base_name(d)?;
        if let Some(fref) = self.frefs.get(name).copied() {
            let args = self.call_args(d)?.to_vec();
            let vals = self.arg_values(&args)?;
            self.bcx.ins().call(fref, &vals);
            return Ok(());
        }
        if name == "ASSERT" && !self.vars.contains_key(name) {
            let args = self.call_args(d)?.to_vec();
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

    /// A call's *value*: `MIN`/`MAX` are constants the checker recorded, the
    /// predeclared functions the fragment uses are IR (`ABS`, `ODD` and the
    /// three shifts), and a procedure of this module is a call.
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
        // A name that is a variable here is a procedure value, so this is a call
        // through a pointer, which needs a signature in the IR (PLAN.md W9.2).
        if self.vars.contains_key(&name) || self.b.vars.contains_key(&name) {
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
            ExprKind::Designator(d) => self.designator_value(d, e.span),
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
        // A designator that names a constant *is* its value: the checker
        // recorded it where it is used, so the folding the C path needs for C's
        // warnings has already happened (R20, PLAN.md W6.2).
        if let Some(c) = self.b.sema.const_at(span) {
            return match c.int() {
                Some(v) => Ok(self.bcx.ins().iconst(I32, i64::from(v))),
                None => Err(not_yet("a non-INTEGER constant", span)),
            };
        }
        let name = self.base_name(d)?;
        match d.sels.as_slice() {
            [] => self.load_var(name, span),
            [Selector::Index(ix)] => {
                let (addr, elem) = self.elem_addr(name, ix, span)?;
                Ok(self.bcx.ins().load(elem, ir::MemFlagsData::new(), addr, 0))
            }
            _ => Err(not_yet(
                &format!("the designator `{name}` with its selectors"),
                span,
            )),
        }
    }

    fn load_var(&mut self, name: &str, span: Span) -> Result<ir::Value, Diagnostic> {
        if let Some(var) = self.vars.get(name) {
            return Ok(self.bcx.use_var(*var));
        }
        if self.slots.contains_key(name) {
            // A whole array where a *value* is expected is a copy, which is
            // PLAN.md W9.2's ("string and record copies"); naming one place
            // refuses both.
            return Err(not_yet(&format!("`{name}` as a value"), span));
        }
        if let Some(gv) = self.gv.get(name) {
            let addr = self.bcx.ins().symbol_value(self.b.ptr, *gv);
            return Ok(self.bcx.ins().load(I32, ir::MemFlagsData::new(), addr, 0));
        }
        Err(not_yet(&format!("the designator `{name}` used as a value"), span))
    }

    fn store_var(&mut self, name: &str, v: ir::Value, span: Span) -> Result<(), Diagnostic> {
        if let Some(var) = self.vars.get(name) {
            self.bcx.def_var(*var, v);
            return Ok(());
        }
        if let Some(gv) = self.gv.get(name) {
            let addr = self.bcx.ins().symbol_value(self.b.ptr, *gv);
            self.bcx.ins().store(ir::MemFlagsData::new(), v, addr, 0);
            return Ok(());
        }
        Err(not_yet(&format!("the designator `{name}`"), span))
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
