//! The C back end (DESIGN 9): walk the checked module and emit C11.
//!
//! Runtime cases checked in `oberon/csmoke` include DIV/MOD, sets, shifts,
//! descriptors, links, and parameter modes. `oberon/golden` checks statement
//! lowering and the module and command model.
//!
//! One Oberon module becomes `Foo.c` + `Foo.h` + `Foo.sym`; the shared runtime
//! is `oberon.h` + `oberon_runtime.c` (the two `.inc` files next to this one),
//! so the registry and the descriptors have one identity across modules.
//!
//! The representation decisions below shape everything else:
//!
//! * A fixed-size `ARRAY n OF T` becomes a **struct with one field**,
//!   `struct { T e[n]; }`. Assignment is then C struct assignment (a copy, no
//!   `memcpy` bookkeeping), passing is passing a pointer, and indexing is
//!   `x.e[i]`. The name is derived from the type's id, so two variables of the
//!   same Oberon array type have the same C type and assign to each other. An
//!   Oberon identifier cannot contain `__`, which makes generated names
//!   collision-free.
//! * An open-array parameter becomes two C parameters, `T *a, int32_t a__len`.
//!   `LEN(a)` reads the hidden length (report 10.1).
//! * A procedure that declares nested procedures stores its variables in a
//!   frame struct. A nested procedure takes a hidden `void *sl` and reaches
//!   enclosing storage by walking links, as verified in `oberon/csmoke`.
//!   A procedure that takes a link cannot be command-shaped.
//!

use std::collections::HashMap;
use std::fmt::Write as _;

use syntax::ast::{self, BinOp, CaseLabel, Expr, ExprKind, Module, QIdent, Selector, Span, Stmt, StmtKind, TypeExpr, UnOp};
use sema::const_eval;
use sema::sema::Sema;
use sema::types::{Basic, Field, Param, Type, TypeId, Types};

/// `oberon.h`, emitted alongside the generated modules.
pub const RUNTIME_HEADER: &str = include_str!("oberon_h.inc");
/// `oberon_runtime.c`, emitted once per program.
pub const RUNTIME_SOURCE: &str = include_str!("oberon_runtime.inc");

const PREDEFINED_PROPER: &[&str] = &["INC", "DEC", "INCL", "EXCL", "NEW", "ASSERT", "PACK", "UNPK"];
const PREDEFINED_FUNCTIONS: &[&str] =
    &["ABS", "ODD", "LEN", "LSL", "ASR", "ROR", "FLOOR", "FLT", "ORD", "CHR"];

/// Report 11.1: function calls return values; proper procedure calls are
/// statements. Neither is called through a signature. See `sys_function` and
/// `sys_proper`.
const SYSTEM_FUNCTIONS: &[&str] = &["ADR", "SIZE", "BIT"];
const SYSTEM_PROPER: &[&str] = &["GET", "PUT", "COPY"];

/// What an identifier means at emission time.
#[derive(Clone)]
struct Sym {
    /// The emitted spelling of the name at this point in the output.
    cname: String,
    ty: TypeId,
    /// Nesting level of the declaring procedure (0 = module).
    level: usize,
    /// A `VAR` parameter is a C pointer, so every use needs a deref.
    by_var: bool,
    /// The narrowed spelling of a symbol whose `cname` is a cast. A type-`CASE`
    /// arm's variable keeps the cast for *field* access, because the cast is what
    /// gives a derived field its layout, but an assignment target or a `VAR`
    /// argument needs the plain designator: a cast is not an lvalue (DESIGN R29).
    plain: Option<String>,
    /// A *declared* procedure (as opposed to a procedure-valued variable or
    /// parameter): only the former takes a static link.
    decl_proc: bool,
}

#[derive(Clone)]
struct Frame {
    /// C name of the frame struct, when the procedure owns one.
    frame: String,
    level: usize,
    /// Mangled lexical path, for generated names.
    path: String,
}

struct SourceMap {
    file: String,
    line_starts: Vec<u32>,
}

impl SourceMap {
    fn new(source: &str, file: &str) -> SourceMap {
        let mut line_starts = vec![0];
        line_starts.extend(
            source
                .bytes()
                .enumerate()
                .filter_map(|(i, b)| (b == b'\n').then_some((i + 1) as u32)),
        );
        SourceMap {
            file: escape_line_file(file),
            line_starts,
        }
    }

    fn directive(&self, span: Span, out: &mut String) {
        let line = self.line_starts.partition_point(|start| *start <= span.lo);
        let _ = writeln!(out, "#line {line} \"{}\"", self.file);
    }
}

fn escape_line_file(file: &str) -> String {
    let mut out = String::new();
    for byte in file.bytes() {
        match byte {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x20..=0x7e => out.push(byte as char),
            _ => {
                let _ = write!(out, "\\{byte:03o}");
            }
        }
    }
    out
}

pub struct Emitter<'a> {
    types: &'a Types,
    sema: &'a Sema<'a>,
    module: String,
    /// C struct names for record and array types.
    type_names: HashMap<TypeId, String>,
    /// Whether a type reaches a pointer, memoized because the walk is
    /// exponential in the depth of a type graph (DESIGN 15.2). This determines
    /// whether a record needs a pointer map and a variable is a root.
    reaches: HashMap<TypeId, bool>,
    /// The Oberon name of every type this module declares, so an interface can
    /// refer to it the way its reader spells it.
    oberon_names: HashMap<TypeId, String>,
    /// Number of unresolved type references. A checked module has none; an
    /// unresolved reference produces a `#error` in the emitted C.
    unresolved: usize,
    /// Import alias -> the module it stands for.
    aliases: HashMap<String, String>,
    /// The name each *declared* type will be given, filled before visiting it.
    /// Otherwise a composite reached through a pointer before its declaration
    /// (`ARRAY 2 OF P`, where `P = POINTER TO R` and `R` comes later in the same
    /// `TYPE` section) would otherwise be named `Anon_n`. Later uses would then
    /// have a different name (DESIGN R32, PLAN.md W2.4).
    declared_names: HashMap<TypeId, String>,
    /// Emitted into the header: struct definitions, constants, externs.
    typedefs: String,
    header: String,
    /// Emitted into the translation unit.
    descriptors: String,
    globals: String,
    protos: String,
    /// After the prototypes, because the command table names them.
    trailer: String,
    scopes: Vec<HashMap<String, Sym>>,
    frames: Vec<Frame>,
    /// Statements hoisted before the construct being emitted (temporaries for
    /// structured value parameters). Flush them where the construct is evaluated,
    /// never out of a loop.
    prelude: String,
    anon: usize,
    /// Non-zero while rendering the right operand of `&&` or `||`, whose value
    /// must not be computed unless the operand is reached.
    in_conditional: usize,
    /// The result type of the current procedure and whether its body returns.
    /// through one site to preserve the single-exit property (D6 in section 5).
    cur_ret: Option<TypeId>,
    ret_label: bool,
    /// Runtime checks (DESIGN 9.4, PLAN.md W3.1). On by default; `-Ochecks=n`
    /// removes index, `NIL`, guard, and type-`CASE` fallback checks. Violations
    /// then become the program's responsibility instead of located traps.
    /// Divisor checks remain to keep C arithmetic defined (R4). `ASSERT` is a
    /// statement that performs a check.
    checks: Checks,
    line_directives: Option<SourceMap>,
}

pub fn emit_module(module: &Module, types: &Types, sema: &Sema<'_>) -> (String, String) {
    emit_module_checked(module, types, sema, Checks::ALL)
}

/// Which runtime checks the emitter writes (PLAN.md W3.1, refined by W8.3). All
/// checks are enabled by default. `-Ochecks=n` disables them, while
/// `-Ochecks=index,guard` keeps the `NIL` check and type-`CASE` fallback.
/// Divisor checks stay enabled to keep C arithmetic defined (R4). `ASSERT`
/// performs a check as a statement.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Checks {
    pub index: bool,
    pub nil: bool,
    pub guard: bool,
    pub case_: bool,
}

impl Checks {
    pub const ALL: Checks = Checks { index: true, nil: true, guard: true, case_: true };
    pub const NONE: Checks = Checks { index: false, nil: false, guard: false, case_: false };

    /// The set named by `-Ochecks=<list>`. `all` and `none` select the whole or
    /// empty set. An unknown name returns `None` so the driver can report it.
    pub fn parse(list: &str) -> Option<Checks> {
        let mut c = Checks::NONE;
        for name in list.split(',') {
            match name.trim() {
                "" => {}
                "all" | "y" => return Some(Checks::ALL),
                "none" | "n" => return Some(Checks::NONE),
                "index" => c.index = true,
                "nil" => c.nil = true,
                "guard" => c.guard = true,
                "case" => c.case_ = true,
                _ => return None,
            }
        }
        Some(c)
    }

    /// The names `parse` accepts, for the driver's message.
    pub const NAMES: &'static str = "index, nil, guard, case, all, none";
}

/// The same, with the runtime checks under the caller's control (`-Ochecks=n`).
pub fn emit_module_checked(
    module: &Module,
    types: &Types,
    sema: &Sema<'_>,
    checks: Checks,
) -> (String, String) {
    let mut e = Emitter::new(module.name.clone(), types, sema);
    e.checks = checks;
    e.run(module)
}

/// Emit the checked module with C line directives for its source statements.
pub fn emit_module_checked_with_lines(
    module: &Module,
    types: &Types,
    sema: &Sema<'_>,
    checks: Checks,
    source: &str,
    file: &str,
) -> (String, String) {
    let mut e = Emitter::new(module.name.clone(), types, sema);
    e.checks = checks;
    e.line_directives = Some(SourceMap::new(source, file));
    e.run(module)
}

impl<'a> Emitter<'a> {
    fn new(module: String, types: &'a Types, sema: &'a Sema<'a>) -> Emitter<'a> {
        Emitter {
            checks: Checks::ALL,
            types,
            sema,
            module,
            type_names: HashMap::new(),
            reaches: HashMap::new(),
            oberon_names: HashMap::new(),
            unresolved: 0,
            aliases: HashMap::new(),
            declared_names: HashMap::new(),
            typedefs: String::new(),
            header: String::new(),
            descriptors: String::new(),
            globals: String::new(),
            protos: String::new(),
            trailer: String::new(),
            scopes: vec![HashMap::new()],
            frames: Vec::new(),
            prelude: String::new(),
            anon: 0,
            in_conditional: 0,
            cur_ret: None,
            ret_label: false,
            line_directives: None,
        }
    }

    // Top-level emission

    fn run(&mut self, module: &Module) -> (String, String) {
        for imp in &module.imports {
            self.aliases.insert(imp.alias.clone(), imp.module.clone());
        }
        // 1. Name each composite type, then emit them in dependency order
        //    (element before array, base before derived). Set declared names
        //    first so pointer references retain them.
        for t in &module.decls.types {
            let ty = self.decl_type(&t.ty);
            self.declared_names.insert(ty, self.cname(&t.name));
        }
        let mut wanted = Vec::new();
        self.collect_types(&module.decls, &mut wanted);
        // Forward-declare every composite. Arrays of pointers can then refer to
        // later record declarations. `type_rank` cannot ensure this because
        // pointers don't constrain the order. C11 permits repeating the typedef
        // with the definition.
        for t in self.type_names.keys().copied().collect::<Vec<_>>() {
            if matches!(self.types.get(t), Type::Array { .. } | Type::Record { .. }) {
                let name = self.type_names[&t].clone();
                let _ = writeln!(self.typedefs, "typedef struct {name} {name};");
            }
        }
        let mut all: Vec<TypeId> = self.type_names.keys().copied().collect();
        all.sort_by_key(|t| self.type_rank(*t));
        for t in all {
            self.emit_type(t);
        }
        // A type name denotes its type when a label or a guard names it.
        for t in &module.decls.types {
            let ty = self.decl_type(&t.ty);
            let cname = self.type_c_name(ty).unwrap_or_default();
            self.declare(&t.name, Sym { cname, ty, level: 0, by_var: false, plain: None, decl_proc: false });
        }
        // Imported types: `M.T` resolves through the other module's header.
        for imp in &module.imports {
            if let Some(idx) = self.sema.interface_index(&imp.module) {
                let env = self.sema.env_of(idx);
                for (name, ty) in env {
                    self.declare(
                        &name,
                        Sym {
                            cname: format!("{}__{}", mangle(&imp.module), mangle(&name)),
                            ty,
                            level: 0,
                            by_var: false, plain: None,
                            decl_proc: false,
                        },
                    );
                }
            }
        }

        // 2. Constants as macros, so a client can use them in array bounds.
        for c in &module.decls.consts {
            let (value, ty) = match self.sema.const_of(c.span) {
                Some((v, t)) => (self.const_cvalue(v), *t),
                // Unreachable: the emitter only runs on modules that check.
                None => (self.const_value(&c.value), TypeId(0)),
            };
            let _ = writeln!(self.header, "#define {} {}", self.cname(&c.name), value);
            self.declare(
                &c.name,
                Sym { cname: self.cname(&c.name), ty, level: 0, by_var: false, plain: None, decl_proc: false },
            );
        }
        // 3. Globals: exported ones have external linkage, the rest are static.
        for v in &module.decls.vars {
            let ty = self.decl_type(&v.ty);
            for (name, export) in &v.names {
                let cname = self.cname(name);
                let decl = self.binding(ty, &cname);
                if *export {
                    let _ = writeln!(self.globals, "{decl};");
                    let _ = writeln!(self.header, "extern {decl};");
                } else {
                    let _ = writeln!(self.globals, "static {decl};");
                }
                self.declare(name, Sym { cname, ty, level: 0, by_var: false, plain: None, decl_proc: false });
            }
        }

        // 4. Procedures follow their declarations. Oberon declares before use,
        //    so C only needs the header's prototypes.
        let mut procs = String::new();
        for p in &module.decls.procs {
            self.procedure(p, &mut procs);
        }

        let mut tail = String::new();
        // 5. Emit the module body and command table (report 11).
        self.frames.push(Frame { frame: String::new(), level: 1, path: "init".into() });
        self.push_scope();
        let _ = writeln!(tail, "void {}__init(void) {{", self.module);
        // Register collector roots before the body can allocate (DESIGN 15.4).
        // Include module variables whose storage can reach the heap, with a map
        // for that storage. Use the pointer slot, record type, or array element
        // map as appropriate. Skip variables whose types contain no pointers.
        // Register roots here because this module defines the storage. Clients
        // don't allocate through exported variables.
        for v in &module.decls.vars {
            let ty = self.decl_type(&v.ty);
            for (name, _) in &v.names {
                let map = self.region_fields(ty, "0");
                if map.is_empty() {
                    continue;
                }
                let cname = self.cname(name);
                let _ = writeln!(self.globals, "static const ObFld {cname}__roots[] = {{");
                for e in &map {
                    let _ = writeln!(self.globals, "    {{ {e} }},");
                }
                let _ = writeln!(self.globals, "}};");
                let _ = writeln!(
                    tail,
                    "    ob_gc_root_region(&{cname}, {cname}__roots, {});",
                    map.len()
                );
            }
        }
        // Mark unexported variables even if the module never reads them. C flags
        // an unreferenced `static` variable under `-Werror` (R20). Reading its
        // zero-initialized value costs the same as the parameter marks (PLAN.md W6.2).
        for v in &module.decls.vars {
            for (name, export) in &v.names {
                if !*export {
                    let _ = writeln!(tail, "    (void){};", self.cname(name));
                }
            }
        }
        for v in &module.decls.vars {
            let ty = self.decl_type(&v.ty);
            for (name, _) in &v.names {
                if self.contains_record(ty) {
                    let cname = self.cname(name);
                    self.desc_init(ty, &cname, &mut tail, "    ");
                }
            }
        }
        for s in &module.body {
            self.stmt(s, &mut tail, 1);
        }
        let _ = writeln!(tail, "}}");
        self.pop_scope();
        self.frames.pop();
        self.commands(module, &mut tail);

        let mut out = String::new();
        let _ = writeln!(out, "/* Generated from {}.Mod. */", self.module);
        if self.unresolved > 0 {
            // A checked module cannot reach this; if it does, say so loudly
            // instead of emitting a plausible-looking wrong type.
            let _ = writeln!(
                out,
                "#error \"oberon compiler: {} unresolved type reference(s) in {}\"",
                self.unresolved, self.module
            );
        }
        let _ = writeln!(out, "#include \"{}.h\"", self.module);
        out.push_str(&self.descriptors);
        out.push_str(&self.globals);
        out.push_str(&self.protos);
        out.push_str(&self.trailer);
        out.push_str(&procs);
        out.push_str(&tail);

        let mut h = String::new();
        let guard = format!("{}_H", self.module.to_uppercase().replace("__", "_"));
        let _ = writeln!(h, "/* Interface of module {} (DESIGN 9.8). */", self.module);
        let _ = writeln!(h, "#ifndef {guard}");
        let _ = writeln!(h, "#define {guard}");
        h.push_str("#include \"oberon.h\"\n");
        // Include imported modules so their types and prototypes are available
        // to header users (DESIGN 9.8). SYSTEM has no header because it is a
        // compiler module (9.10). The emitted `.sym` records SYSTEM imports.
        for imp in &module.imports {
            if imp.module == "SYSTEM" {
                continue;
            }
            let _ = writeln!(h, "#include \"{}.h\"", imp.module);
        }
        h.push_str(&self.typedefs);
        h.push_str(&self.header);
        let _ = writeln!(h, "void {}__init(void);", self.module);
        let _ = writeln!(h, "void {}__load(void);", self.module);
        let _ = writeln!(h, "#endif /* {guard} */");
        (out, h)
    }

    fn commands(&mut self, m: &Module, out: &mut String) {
        let mut entries = Vec::new();
        for p in &m.decls.procs {
            // Only parameterless procedures are commands. The `void (*)(void)`
            // entry matches their signature (report 11, risk R12).
            if p.export && p.params.is_empty() && p.result.is_none() {
                entries.push((p.name.clone(), format!("{}__{}", self.module, mangle(&p.name))));
            }
        }
        let _ = writeln!(self.trailer, "static const ObCommand {}__commands[] = {{", self.module);
        for (name, cname) in &entries {
            let _ = writeln!(self.trailer, "    {{ \"{name}\", {cname} }},");
        }
        let _ = writeln!(self.trailer, "    {{ NULL, NULL }}");
        let _ = writeln!(self.trailer, "}};");
        let _ = writeln!(out, "void {}__load(void) {{", self.module);
        let _ = writeln!(out, "    {}__init();", self.module);
        let _ = writeln!(out, "    ob_register_commands(\"{}\", {}__commands);", self.module, self.module);
        let _ = writeln!(out, "}}");
    }

    // Type emission

    fn collect_types(&mut self, decls: &ast::Decls, _wanted: &mut Vec<TypeId>) {
        for t in &decls.types {
            let ty = self.decl_type(&t.ty);
            self.oberon_names.insert(ty, t.name.clone());
            self.name_type(ty, &self.cname(&t.name));
        }
        for v in &decls.vars {
            let ty = self.decl_type(&v.ty);
            self.name_type(ty, "");
        }
        for p in &decls.procs {
            for sec in &p.params {
                // The checker records the flattened FPSection type under the
                // procedure's span, which is also the lookup key here.
                let key = span_of_type(&TypeExpr::Named(sec.ty.base.clone(), p.span));
                if let Some(t) = self.sema.type_at(key, self.types) {
                    self.name_type(t, "");
                }
            }
            self.collect_types(&p.decls, _wanted);
        }
    }

    fn name_type(&mut self, t: TypeId, declared: &str) {
        // A type from an interface file belongs to that module and its header
        // defines it. Naming it here would create a second, incompatible C type
        // (DESIGN 9.8).
        if self.sema.qualified_name(t).is_some() {
            return;
        }
        // Name a composite before visiting its dependencies to handle recursive
        // types. For `P = POINTER TO R; R = RECORD next: P END`, this finds R
        // already named when it returns through P.
        match self.types.get(t).clone() {
            Type::Array { elem, .. } => {
                let name = if declared.is_empty() {
                    match self.declared_names.get(&t) {
                        Some(n) => n.clone(),
                        None => format!("{}__Arr_{}", self.module, t.0),
                    }
                } else {
                    declared.to_string()
                };
                // Check *before* inserting: `insert` replaces the value and
                // only then returns the old one, which would rename the type.
                if self.type_names.contains_key(&t) {
                    return;
                }
                self.type_names.insert(t, name);
                self.name_type(elem, "");
            }
            Type::Record { base, fields, .. } => {
                let name = if declared.is_empty() {
                    match self.declared_names.get(&t) {
                        Some(n) => n.clone(),
                        None => {
                            self.anon += 1;
                            format!("{}__Anon_{}", self.module, self.anon)
                        }
                    }
                } else {
                    declared.to_string()
                };
                if self.type_names.contains_key(&t) {
                    return;
                }
                self.type_names.insert(t, name);
                if let Some(b) = base {
                    let bname = self.type_c_name(b).unwrap_or_default();
                    self.name_type(b, &bname);
                }
                for f in &fields {
                    self.name_type(f.ty, "");
                }
            }
            Type::Pointer { base } => {
                // This map supplies a declared name when the pointee is not
                // named yet. Use it before trying the generated C name.
                let bname = self
                    .declared_names
                    .get(&base)
                    .cloned()
                    .or_else(|| self.type_c_name(base))
                    .unwrap_or_default();
                self.name_type(base, &bname);
            }
            Type::Proc { params, result } => {
                for p in &params {
                    self.name_type(p.ty, "");
                }
                if let Some(r) = result {
                    self.name_type(r, "");
                }
            }
            Type::Basic(_) | Type::OpenArray { .. } | Type::Nil | Type::StringLit(_) | Type::Error => {}
        }
    }

    /// Element before array, base before derived: one pass can emit them all.
    fn type_rank(&self, t: TypeId) -> u32 {
        match self.types.get(t) {
            Type::Array { elem, .. } => 100 + self.type_rank(*elem),
            Type::Record { base, fields, .. } => {
                // By-value fields and embedded bases must be complete first.
                // Pointer fields don't constrain the order.
                let mut rank = 200;
                if let Some(b) = base {
                    rank = rank.max(200 + self.type_rank(*b));
                }
                for f in fields {
                    rank = rank.max(300 + self.type_rank(f.ty));
                }
                rank
            }
            _ => 0,
        }
    }

    fn emit_type(&mut self, t: TypeId) {
        let name = match self.type_names.get(&t) {
            Some(n) => n.clone(),
            None => return,
        };
        match self.types.get(t).clone() {
            Type::Array { len, elem } => {
                let et = self.c_type(elem);
                let _ = writeln!(self.typedefs, "typedef struct {name} {name};");
                let _ = writeln!(self.typedefs, "struct {name} {{ {et} e[{len}]; }};");
            }
            Type::Record { base, fields, .. } => {
                let _ = writeln!(self.typedefs, "typedef struct {name} {name};");
                let _ = writeln!(self.typedefs, "struct {name} {{");
                // The descriptor is stored in the first record in the base
                // chain (verified in `oberon/csmoke`). Every derived record
                // begins with its base, so `*(ObDesc **)p` reads the same word
                // through a `Base*` or `Derived*`. `IS`, guards, and type CASE
                // use that shared descriptor.
                match base {
                    Some(b) => {
                        // An imported base type uses its module's namespace.
                        let bname = self.type_c_name(b).unwrap_or_else(|| "void".into());
                        let _ = writeln!(self.typedefs, "    {bname} base;");
                    }
                    None => {
                        let _ = writeln!(self.typedefs, "    const ObDesc *desc;");
                    }
                }
                for f in &fields {
                    let _ = writeln!(
                        self.typedefs,
                        "    {};",
                        self.binding(f.ty, &mangle(&f.name))
                    );
                }
                let _ = writeln!(self.typedefs, "}};");
                let base_ref = match base {
                    Some(b) => match self.type_c_name(b) {
                        Some(bn) => format!("&TD_{bn}"),
                        None => "NULL".into(),
                    },
                    None => "NULL".into(),
                };
                // The collector's pointer map (DESIGN 15.2) uses the same walk
                // as `desc_init` to describe nested records. Types without
                // pointers have no map.
                let map = self.record_fields(t);
                let fields_ref = if map.is_empty() {
                    "NULL".into()
                } else {
                    let _ = writeln!(self.descriptors, "static const ObFld {name}__fields[] = {{");
                    for e in &map {
                        let _ = writeln!(self.descriptors, "    {{ {e} }},");
                    }
                    let _ = writeln!(self.descriptors, "}};");
                    format!("{name}__fields")
                };
                let _ = writeln!(
                    self.descriptors,
                    "extern const ObDesc TD_{name};\nconst ObDesc TD_{name} = {{ {base_ref}, {}u, \"{name}\", {fields_ref}, {} }};",
                    t.0,
                    map.len()
                );
                let _ = writeln!(self.header, "extern const ObDesc TD_{name};");
            }
            _ => {}
        }
    }

    // C bindings

    fn binding(&self, t: TypeId, name: &str) -> String {
        // A procedure type is a declarator: place the variable name inside it,
        // as in `int32_t (*f)(int32_t)`.
        if let Type::Proc { params, result } = self.types.get(t) {
            let r = match result {
                Some(r) => self.c_type(*r),
                None => "void".into(),
            };
            let mut args = Vec::new();
            for p in params {
                if let Type::OpenArray { elem } = self.types.get(p.ty) {
                    args.push(format!("{} *", self.c_type(*elem)));
                    args.push("int32_t".into());
                } else if p.by_var {
                    args.push(format!("{} *", self.c_type(p.ty)));
                } else {
                    args.push(self.c_type(p.ty));
                }
            }
            let list = if args.is_empty() { "void".to_string() } else { args.join(", ") };
            return format!("{r} (*{name})({list})");
        }
        format!("{} {name}", self.c_type(t))
    }

    /// The C name of a composite type: this module's own, or the defining
    /// module's for an imported one.
    fn type_c_name(&self, t: TypeId) -> Option<String> {
        if let Some(n) = self.type_names.get(&t) {
            return Some(n.clone());
        }
        self.sema.qualified_name(t).map(|q| mangle(&q))
    }

    fn c_type(&self, t: TypeId) -> String {
        match self.types.get(t) {
            Type::Basic(Basic::Integer) | Type::Basic(Basic::Byte) => "int32_t".into(),
            Type::Basic(Basic::Real) => "float".into(),
            Type::Basic(Basic::Char) => "char".into(),
            Type::Basic(Basic::Boolean) => "int32_t".into(),
            Type::Basic(Basic::Set) => "uint32_t".into(),
            Type::Array { .. } | Type::Record { .. } => {
                self.type_c_name(t).unwrap_or_else(|| "void *".into())
            }
            Type::OpenArray { .. } => "void *".into(),
            Type::Pointer { base } => match self.type_c_name(*base) {
                Some(n) => format!("{n} *"),
                None => "void *".into(),
            },
            Type::Proc { params, result } => {
                let r = match result {
                    Some(r) => self.c_type(*r),
                    None => "void".into(),
                };
                let mut args = Vec::new();
                for p in params {
                    if let Type::OpenArray { elem } = self.types.get(p.ty) {
                        args.push(format!("{} *", self.c_type(*elem)));
                        args.push("int32_t".into());
                    } else if p.by_var {
                        args.push(format!("{} *", self.c_type(p.ty)));
                    } else {
                        args.push(self.c_type(p.ty));
                    }
                }
                if args.is_empty() {
                    format!("{r} (*)(void)")
                } else {
                    format!("{r} (*)({})", args.join(", "))
                }
            }
            Type::Nil => "void *".into(),
            Type::StringLit(_) => "const char *".into(),
            Type::Error => "int32_t".into(),
        }
    }

    /// The type of a declaration's `TypeExpr`, recorded by the semantic pass
    /// under its span (DESIGN 6).
    fn decl_type(&self, t: &TypeExpr) -> TypeId {
        self.sema.type_at(span_of_type(t), self.types).unwrap_or(TypeId(0))
    }

    // Scope helpers

    fn declare(&mut self, name: &str, sym: Sym) {
        self.scopes.last_mut().expect("a scope").insert(name.to_string(), sym);
    }
    fn lookup(&self, name: &str) -> Option<Sym> {
        self.scopes.iter().rev().find_map(|s| s.get(name).cloned())
    }
    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }
    fn pop_scope(&mut self) {
        self.scopes.pop();
    }
    fn cname(&self, name: &str) -> String {
        format!("{}__{}", self.module, mangle(name))
    }

    // Procedure emission

    fn procedure(&mut self, p: &ast::Proc, out: &mut String) {
        let (outer_ret, outer_label) = (self.cur_ret, self.ret_label);
        let level = self.frames.len() + 1;
        let path = match self.frames.last() {
            Some(f) if level > 1 => format!("{}__{}", f.path, p.name),
            _ => p.name.clone(),
        };
        let cname = format!("{}__{}", self.module, mangle(&path));
        let has_nested = !p.decls.procs.is_empty();
        let frame = format!("F_{}", mangle(&path));

        // The signature as the checker flattened it, and its procedure type.
        let (pty, flat, result) = match self.sema.signatures.get(&sema::sema::span_key(p.span)) {
            Some(sig) => (sig.ty, sig.params.clone(), sig.result),
            None => (TypeId(0), Vec::new(), None),
        };
        // Declare it before the body, so recursion and later calls resolve.
        self.declare(
            &p.name,
            Sym { cname: cname.clone(), ty: pty, level, by_var: false, plain: None, decl_proc: true },
        );

        let ret = match result {
            Some(r) => self.c_type(r),
            None => "void".into(),
        };

        let mut args: Vec<String> = Vec::new();
        let mut proto_args: Vec<String> = Vec::new();
        if level > 1 {
            // A nested procedure takes a static link (report 6.5, DESIGN 9.7).
            args.push("void *sl".into());
            proto_args.push("void *".into());
        }
        for (n, pm) in &flat {
            let name = mangle(n);
            match self.types.get(pm.ty) {
                Type::OpenArray { elem } => {
                    args.push(format!("{} *{name}", self.c_type(*elem)));
                    proto_args.push(format!("{} *", self.c_type(*elem)));
                    args.push(format!("int32_t {name}__len"));
                    proto_args.push("int32_t".into());
                }
                _ if self.param_is_pointer(pm) => {
                    args.push(format!("{} *{name}", self.c_type(pm.ty)));
                    proto_args.push(format!("{} *", self.c_type(pm.ty)));
                }
                _ => {
                    args.push(self.binding(pm.ty, &name));
                    proto_args.push(self.c_type(pm.ty));
                }
            }
        }
        let proto = if proto_args.is_empty() { "void".to_string() } else { proto_args.join(", ") };
        let _ = writeln!(self.protos, "{ret} {cname}({proto});");
        if p.export {
            let _ = writeln!(self.header, "{ret} {cname}({proto});");
        }

        self.push_scope();

        // The frame, when nested procedures must reach this procedure's storage.
        if has_nested {
            let _ = writeln!(self.typedefs, "typedef struct {frame} {frame};");
            let _ = writeln!(self.typedefs, "struct {frame} {{");
            let _ = writeln!(self.typedefs, "    void *sl;");
            for (n, pm) in &flat {
                let name = mangle(n);
                match self.types.get(pm.ty) {
                    Type::OpenArray { elem } => {
                        let _ = writeln!(self.typedefs, "    {} *{name};", self.c_type(*elem));
                        let _ = writeln!(self.typedefs, "    int32_t {name}__len;");
                    }
                    _ if self.param_is_pointer(pm) => {
                        let _ = writeln!(self.typedefs, "    {} *{name};", self.c_type(pm.ty));
                    }
                    _ => {
                        let _ = writeln!(self.typedefs, "    {};", self.binding(pm.ty, &name));
                    }
                }
            }
            for v in &p.decls.vars {
                let ty = self.decl_type(&v.ty);
                for (n, _) in &v.names {
                    let _ = writeln!(self.typedefs, "    {};", self.binding(ty, &mangle(n)));
                }
            }
            let _ = writeln!(self.typedefs, "}};");
        }

        let def_args = if args.is_empty() { "void".to_string() } else { args.join(", ") };
        let _ = writeln!(out, "{ret} {cname}({def_args}) {{");
        if has_nested {
            let _ = writeln!(out, "    {frame} f;");
            let _ = writeln!(out, "    f.sl = {};", if level > 1 { "sl" } else { "NULL" });
        }
        if level > 1 {
            // The link may be unused when the body reaches nothing enclosing.
            let _ = writeln!(out, "    (void)sl;");
        }
        // Oberon does not require a body to read its parameters, and the same
        // build line rejects `-Wunused-parameter`/`-Wunused-but-set-parameter`
        // (R20): mark each one used. Nested procedures copy their parameters
        // into a frame, which does read them, but the extra statement costs
        // nothing and removes the class of failure that the generator found.
        for (n, pm) in &flat {
            let name = mangle(n);
            let _ = writeln!(out, "    (void){name};");
            if matches!(self.types.get(pm.ty), Type::OpenArray { .. }) {
                let _ = writeln!(out, "    (void){name}__len;");
            }
        }

        self.frames.push(Frame { frame: frame.clone(), level, path: path.clone() });

        for (n, pm) in &flat {
            let name = mangle(n);
            let local = if has_nested { format!("f.{name}") } else { name.clone() };
            if has_nested {
                // A value parameter is a copy the caller made; a VAR parameter
                // is a pointer and must not be cleared.
                if !pm.by_var {
                    let _ = writeln!(out, "    memset(&f.{name}, 0, sizeof(f.{name}));");
                }
            }
            // A record value parameter is a pointer to the caller's copy, so
            // every use of it dereferences, exactly like a VAR parameter.
            self.declare(
                n,
                Sym {
                    cname: local,
                    ty: pm.ty,
                    level,
                    by_var: self.param_is_pointer(pm),
                    plain: None,
                    decl_proc: false,
                },
            );
            if has_nested {
                let _ = writeln!(out, "    f.{name} = {name};");
                if !self.param_is_pointer(pm) && self.contains_record(pm.ty) {
                    self.desc_init(pm.ty, &format!("f.{name}"), out, "    ");
                }
                if let Type::OpenArray { .. } = self.types.get(pm.ty) {
                    let _ = writeln!(out, "    f.{name}__len = {name}__len;");
                }
            }
        }

        for t in &p.decls.types {
            let maybe = self.decl_type(&t.ty);
            let cname = self.type_c_name(maybe).unwrap_or_default();
            self.oberon_names.insert(maybe, t.name.clone());
            self.declare(&t.name, Sym { cname, ty: maybe, level, by_var: false, plain: None, decl_proc: false });
        }
        for c in &p.decls.consts {
            let (value, ty) = match self.sema.const_of(c.span) {
                Some((v, t)) => (self.const_cvalue(v), *t),
                None => (self.const_value(&c.value), TypeId(0)),
            };
            let _ = writeln!(out, "    static const {} {} = {value};", self.c_type(ty), mangle(&c.name));
            self.declare(
                &c.name,
                Sym { cname: mangle(&c.name), ty, level, by_var: false, plain: None, decl_proc: false },
            );
        }
        for v in &p.decls.vars {
            let ty = self.decl_type(&v.ty);
            for (n, _) in &v.names {
                let local = mangle(n);
                // The storage is a plain local, or a field of the frame when
                // nested procedures can reach it; either way it starts zeroed
                // and any record inside it needs its static descriptor.
                let place = if has_nested {
                    format!("f.{local}")
                } else {
                    let _ = writeln!(out, "    {};", self.binding(ty, &local));
                    local.clone()
                };
                let _ = writeln!(out, "    memset(&{place}, 0, sizeof({place}));");
                if self.contains_record(ty) {
                    self.desc_init(ty, &place, out, "    ");
                }
                self.declare(
                    n,
                    Sym {
                        cname: if has_nested { format!("f.{local}") } else { local },
                        ty,
                        level,
                        by_var: false, plain: None,
                        decl_proc: false,
                    },
                );
            }
        }
        // A nested procedure is a C function like any other: it is emitted
        // *after* its enclosing procedure's body (its prototype is already in
        // the file), never inside it.
        let mut nested = String::new();
        for q in &p.decls.procs {
            self.procedure(q, &mut nested);
        }
        self.cur_ret = result;
        self.ret_label = false;
        if let Some(want) = result {
            // The one place a function's value leaves from, and zero-initialised
            // so that a path which does not return yields a defined value rather
            // than whatever the frame held.
            let decl = self.binding(want, "ret__acc");
            let _ = writeln!(out, "    {decl};");
            let _ = writeln!(out, "    memset(&ret__acc, 0, sizeof(ret__acc));");
        }
        for s in &p.body {
            self.stmt(s, out, level);
        }
        if self.ret_label {
            if result.is_some() {
                let _ = writeln!(out, "    ret: return ret__acc;");
            } else {
                let _ = writeln!(out, "    ret: return;");
            }
        }
        self.cur_ret = outer_ret;
        self.ret_label = outer_label;
        let _ = writeln!(out, "}}");
        out.push_str(&nested);

        self.frames.pop();
        self.pop_scope();
    }

    /// The type a name refers to in this scope: one declared in the current
    /// procedure or module, or imported as `M.T`.
    fn qident_type(&self, q: &QIdent) -> Option<TypeId> {
        if q.len() == 2 {
            let module = self.module_of_alias(&q[0])?;
            let idx = self.sema.interface_index(&module)?;
            return self.sema.env_of(idx).get(&q[1]).copied();
        }
        if q.len() != 1 {
            return None;
        }
        self.lookup(&q[0]).map(|s| s.ty)
    }

    // Statement emission

    fn take_prelude(&mut self) -> String {
        std::mem::take(&mut self.prelude)
    }

    /// Fixes a value in the prelude and returns the name used for it.
    ///
    /// Oberon's reference compilers evaluate operands left to right, but C leaves
    /// the order of `+`, `*`, `/`, and call arguments unspecified. `m0 + R(3)`
    /// may differ when `R` writes `m0`. Fixing both operands in source order
    /// preserves the language's evaluation order (DESIGN R23, PLAN.md W6.2).
    fn materialise(&mut self, t: TypeId, value: &str) -> String {
        self.anon += 1;
        let tmp = format!("tmp__{}", self.anon);
        let decl = self.binding(t, &tmp);
        if self.in_conditional > 0 {
            // Inside a `&&`/`||` operand, compute the value only when that
            // operand is reached. Hoist only the declaration; keep the
            // assignment in a comma expression, which C sequences.
            let _ = writeln!(self.prelude, "    {decl};");
            format!("(({tmp} = {value}), {tmp})")
        } else {
            let _ = writeln!(self.prelude, "    {decl} = {value};");
            tmp
        }
    }

    /// The same, for something that has to be fixed as an address (a string or
    /// an array operand of a comparison), which sequences the index expressions
    /// inside it.
    fn materialise_addr(&mut self, c_ty: &str, value: &str) -> String {
        self.anon += 1;
        let tmp = format!("tmp__{}", self.anon);
        if self.in_conditional > 0 {
            let _ = writeln!(self.prelude, "    {c_ty} *{tmp};");
            format!("(({tmp} = {value}), {tmp})")
        } else {
            let _ = writeln!(self.prelude, "    {c_ty} *{tmp} = {value};");
            tmp
        }
    }

    /// A value that has no C assignment: an array (a record never reaches the
    /// operators that sequence operands, and an open array is a pointer).
    fn is_aggregate(&self, t: TypeId) -> bool {
        matches!(self.types.get(t), Type::Array { .. })
    }

    fn stmt(&mut self, s: &Stmt, out: &mut String, level: usize) {
        if let Some(source_map) = &self.line_directives {
            source_map.directive(s.span, out);
        }
        match &s.kind {
            StmtKind::Assign { target, value } => {
                let dst = self.designator(target, level, true);
                let dst_ty0 = self.sema.type_at(target.span, self.types);
                let src_ty0 = self.sema.type_at(value.span, self.types);
                let pre = self.take_prelude();
                out.push_str(&pre);
                let dst_ty = self.sema.type_at(target.span, self.types);
                // A string value assigned to a CHAR array needs a padded copy
                // (report 9.1.2). Imported constants retain their designator in
                // the AST, so use the checker's folded value as well.
                let string_value = match &value.kind {
                    ExprKind::Str(text) => Some(text.clone()),
                    _ => match self.sema.const_at(value.span) {
                        Some(sema::const_eval::Const::Str(text)) => Some(text),
                        _ => None,
                    },
                };
                if let (Some(dt), Some(text)) = (dst_ty, string_value.as_deref()) {
                    if let Type::Array { len, elem } = self.types.get(dt) {
                        if self.types.is_char(*elem) {
                            let _ = writeln!(
                                out,
                                "    rt_strcopy({dst}.e, {len}, \"{}\");",
                                escape_c(text)
                            );
                            return;
                        }
                    }
                }
                // Assigning an extended record to a base record must keep the
                // destination's static descriptor. A plain C assignment would
                // copy the source descriptor and give the base variable its
                // derived type (DESIGN 9.3).
                if let (Some(d), Some(sr)) = (dst_ty0, src_ty0) {
                    if self.types.is_record(d) && self.types.is_record(sr) && !self.types.same_type(d, sr) {
                        let pre = self.take_prelude();
                        let src = self.expr(value, None);
                        let pre2 = self.take_prelude();
                        out.push_str(&pre);
                        out.push_str(&pre2);
                        let desc = self.descriptor_name(d);
                        let _ = writeln!(
                            out,
                            "    rt_rec_copy(&({dst}), &({src}), (int32_t)sizeof({dst}), {desc});"
                        );
                        return;
                    }
                }
                // A full-length array-to-array copy is C struct assignment and
                // doesn't write a terminator (risk R14).
                let rhs = self.assign_rhs(value, dst_ty, level);
                let pre2 = self.take_prelude();
                out.push_str(&pre2);
                let _ = writeln!(out, "    {dst} = {rhs};");
            }
            StmtKind::Call(d) => {
                let mut text = String::new();
                self.call(d, level, &mut text);
                let pre = self.take_prelude();
                out.push_str(&pre);
                out.push_str(&text);
            }
            StmtKind::If { arms, els } => {
                // The chain is *nested* rather than spelled `else if`: a guard
                // may need a temporary, and a statement may not sit between `}`
                // and `else`. Nesting puts each branch's setup where that branch
                // is evaluated (or not).
                let mut text = String::new();
                for (i, (cond, body)) in arms.iter().enumerate() {
                    let c = self.expr(cond, Some(self.types.boolean()));
                    let pre = self.take_prelude();
                    text.push_str(&pre);
                    if i == 0 {
                        let _ = writeln!(text, "    if ({c}) {{");
                    } else {
                        let _ = writeln!(text, "    else {{");
                        let _ = writeln!(text, "    if ({c}) {{");
                    }
                    for s in body {
                        self.stmt(s, &mut text, level);
                    }
                    let _ = writeln!(text, "    }}");
                }
                if let Some(b) = els {
                    let _ = writeln!(text, "    else {{");
                    for s in b {
                        self.stmt(s, &mut text, level);
                    }
                    let _ = writeln!(text, "    }}");
                }
                for _ in 1..arms.len() {
                    let _ = writeln!(text, "    }}");
                }
                out.push_str(&text);
            }
            StmtKind::While { arms } => {
                // Repeat until a guard is true. Recreate each guard's temporary
                // inside the loop on every iteration.
                let _ = writeln!(out, "    for (;;) {{");
                for (i, (cond, body)) in arms.iter().enumerate() {
                    let c = self.expr(cond, Some(self.types.boolean()));
                    let pre = self.take_prelude();
                    out.push_str(&pre);
                    let kw = if i == 0 { "if" } else { "else if" };
                    let _ = writeln!(out, "        {kw} ({c}) {{");
                    for s in body {
                        self.stmt(s, out, level);
                    }
                    let _ = writeln!(out, "        }}");
                }
                let _ = writeln!(out, "        else break;");
                let _ = writeln!(out, "    }}");
            }
            StmtKind::Repeat { body, until } => {
                // An explicit loop keeps the condition's temporary inside it.
                let _ = writeln!(out, "    for (;;) {{");
                for s in body {
                    self.stmt(s, out, level);
                }
                let c = self.expr(until, Some(self.types.boolean()));
                let pre = self.take_prelude();
                out.push_str(&pre);
                let _ = writeln!(out, "        if ({c}) break;");
                let _ = writeln!(out, "    }}");
            }
            // `EXIT` (PLAN.md W8.5) uses C's `break`, which exits the innermost
            // loop. Every emitted loop is `for (;;)` or a counted C `for`, so
            // no extra bookkeeping is needed.
            StmtKind::Exit => {
                let _ = writeln!(out, "    break;");
            }
            // Store each return value in the accumulator, then jump to the
            // procedure's single exit point (section 5 D6, PLAN.md W8.4).
            StmtKind::Return(e) => {
                self.ret_label = true;
                if let (Some(e), Some(want)) = (e, self.cur_ret) {
                    let v = self.expr(e, Some(want));
                    let pre = self.take_prelude();
                    out.push_str(&pre);
                    let _ = writeln!(out, "    ret__acc = {v};");
                }
                let _ = writeln!(out, "    goto ret;");
            }
            StmtKind::For { var, from, to, by, body } => {
                let v = self.designator_name(var, level);
                let b = self.expr(from, None);
                let e = self.expr(to, None);
                let step = match by {
                    Some(k) => self.expr(k, None),
                    None => "1".into(),
                };
                let pre = self.take_prelude();
                out.push_str(&pre);
                let _ = writeln!(
                    out,
                    "    for ({v} = {b}; ({step}) > 0 ? {v} <= {e} : {v} >= {e}; {v} += ({step})) {{"
                );
                for s in body {
                    self.stmt(s, out, level);
                }
                let _ = writeln!(out, "    }}");
            }
            StmtKind::Case { subject, arms } => self.case_stmt(subject, arms, out, level),
        }
    }

    fn designator_name(&self, name: &str, level: usize) -> String {
        match self.lookup(name) {
            Some(s) => self.access(&s, level),
            None => mangle(name),
        }
    }

    fn case_stmt(&mut self, subject: &Expr, arms: &[ast::CaseArm], out: &mut String, level: usize) {
        let sub_ty = self.sema.type_at(subject.span, self.types).unwrap_or(TypeId(0));
        let is_type_case = self.types.is_pointer(sub_ty) || self.types.is_record(sub_ty);
        let subj = self.expr(subject, None);
        let pre = self.take_prelude();
        out.push_str(&pre);
        if is_type_case {
            // DESIGN 9.3: switch on the descriptor id. Each arm's local variable
            // has the label's type (report 9.5).
            let desc = self.dyn_of(&subj, sub_ty);
            // A NIL subject is of no type, so it matches no label: id 0 is the
            // "no type" sentinel and no label carries it, which leaves the default
            // arm or the located trap to answer (W2.5, catalogue row 39). Reading
            // the descriptor through NIL, as this used to, faults with no message.
            if self.types.is_pointer(sub_ty) {
                // Cast to the switch's own type: the labels are unsigned, and a
                // bare `0u` in the `?:` would change the operand's signedness,
                // which the documented build line rejects.
                let _ = writeln!(
                    out,
                    "    switch ((uint32_t)(({subj}) != NULL ? ({desc})->id : 0)) {{"
                );
            } else {
                let _ = writeln!(out, "    switch (({desc})->id) {{");
            }
            let mut default_taken = false;
            for arm in arms {
                let labels: Vec<TypeId> = arm
                    .labels
                    .iter()
                    .flatten()
                    .filter_map(|l| match &l.lo {
                        CaseLabel::Named(q) => self.qident_type(q),
                        _ => None,
                    })
                    .collect();
                if labels.is_empty() {
                    let _ = writeln!(out, "    default: {{");
                    default_taken = true;
                } else {
                    let mut line = String::from("    ");
                    for t in &labels {
                        let rec = if self.types.is_pointer(*t) {
                            self.types.deref(*t).unwrap_or(*t)
                        } else {
                            *t
                        };
                        let _ = write!(line, "case {}u: ", rec.0);
                    }
                    let _ = writeln!(line, "{{");
                    out.push_str(&line);
                }
                self.push_scope();
                if let ExprKind::Designator(d) = &subject.kind {
                    let ty = labels.first().copied().unwrap_or(TypeId(0));
                    let narrowed = if self.types.is_pointer(sub_ty) && self.types.is_pointer(ty) {
                        format!("((({}){subj}))", self.c_type(ty))
                    } else if self.types.is_pointer(sub_ty) {
                        format!("(*({} *)(void *)({subj}))", self.c_type(ty))
                    } else {
                        format!("(*(({} *)(void *)&({subj})))", self.c_type(ty))
                    };
                    self.declare(
                        &d.base[0],
                        Sym {
                            cname: narrowed,
                            ty,
                            level,
                            by_var: false,
                            plain: Some(subj.clone()),
                            decl_proc: false,
                        },
                    );
                }
                for s in &arm.body {
                    self.stmt(s, out, level);
                }
                self.pop_scope();
                let _ = writeln!(out, "    break; }}");
            }
            if !default_taken && self.checks.case_ {
                let _ = writeln!(
                    out,
                    "    default: rt_trap_pos(\"CASE: no matching type\", {}); break;",
                    subject.span.lo
                );
            }
            let _ = writeln!(out, "    }}");
            return;
        }
        let _ = writeln!(out, "    switch ({subj}) {{");
        let mut default_taken = false;
        for arm in arms {
            let mut labels: Vec<i32> = Vec::new();
            for l in arm.labels.iter().flatten() {
                let lo = self.case_label(&l.lo);
                let hi = l.hi.as_ref().map(|h| self.case_label(h)).unwrap_or(lo);
                let mut v = lo;
                loop {
                    labels.push(v);
                    if v == hi {
                        break;
                    }
                    v += 1;
                }
            }
            if labels.is_empty() {
                if default_taken {
                    // A second label-less arm is unreachable in Oberon too.
                    // C permits only one `default`.
                    let _ = writeln!(out, "    {{");
                } else {
                    default_taken = true;
                    let _ = writeln!(out, "    default: {{");
                }
            } else {
                // Every label of the arm shares the arm's single block: closing
                // one block per label would unbalance the braces.
                let mut line = String::from("    ");
                for v in &labels {
                    let _ = write!(line, "case {v}: ");
                }
                let _ = writeln!(line, "{{");
                out.push_str(&line);
            }
            for s in &arm.body {
                self.stmt(s, out, level);
            }
            let _ = writeln!(out, "    break; }}");
        }
        // A value matching no label traps, as in the type CASE above (W2.5).
        // This avoids silent fall-through with an undefined result.
        // `self.checks.case_` controls the fallback. `-Ochecks=...` disables it.
        if !default_taken && self.checks.case_ {
            let _ = writeln!(
                out,
                "    default: rt_trap_pos(\"CASE: no matching label\", {}); break;",
                subject.span.lo
            );
        }
        let _ = writeln!(out, "    }}");
    }

    /// The record descriptor for a designator: read the first payload word for a
    /// POINTER, or the first storage word for a record.
    fn dyn_of(&self, subj: &str, ty: TypeId) -> String {
        if self.types.is_pointer(ty) {
            format!("(*(const ObDesc *const *)(const void *)({subj}))")
        } else {
            format!("(*(const ObDesc *const *)(const void *)&({subj}))")
        }
    }

    fn case_label(&self, l: &CaseLabel) -> i32 {
        match l {
            CaseLabel::Int(v) => *v as i32,
            CaseLabel::Char(c) => *c as i32,
            CaseLabel::Str(s) => s.as_bytes().first().copied().unwrap_or(0) as i32,
            CaseLabel::Named(_) => 0,
        }
    }

    /// Adjusts the assignment's right-hand side where Oberon and C types differ
    /// (DESIGN 9.6). A derived record assigned to a base destination needs the
    /// base prefix. Distinct pointer types need a cast.
    fn assign_rhs(&mut self, value: &Expr, dst_ty: Option<TypeId>, level: usize) -> String {
        let dst = dst_ty.unwrap_or(TypeId(0));
        let src = self.sema.type_at(value.span, self.types).unwrap_or(TypeId(0));
        let want = if self.types.is_char(dst) { Some(dst) } else { None };
        let v = self.expr(value, want);
        if self.types.is_record(dst) && self.types.is_record(src) && !self.types.same_type(dst, src) {
            let name = self.c_type(dst);
            return format!("*({name} *)(const void *)&({v})");
        }
        if self.types.is_pointer(dst) && self.types.is_pointer(src) && !self.types.same_type(dst, src) {
            return format!("(({}){v})", self.c_type(dst));
        }
        let _ = level;
        v
    }

    // Expression emission

    fn expr(&mut self, e: &Expr, want: Option<TypeId>) -> String {
        // The C compiler would fold constants anyway, but it also warns about
        // overflow (`-Woverflow`) and tautological comparisons. The documented
        // build treats warnings as errors (R20, PLAN.md W6.2). The folder is
        // checked against the runtime (DESIGN 8.3), so results match helpers.
        // The resolver's use-site table folds designators that name constants.
        // This turns `MAX * (1 + c0)` into one literal, avoiding C overflow.
        // Fold only structurally constant trees (`foldable`). Builtins such as
        // `ABS` may be shadowed by a nested procedure or parameter. Calls may
        // also have side effects, so leave both for emitted C. Fold only INTEGER
        // or BOOLEAN values in their own C type. `Const::int()` would emit a CHAR
        // as `int32_t`, causing a narrowing assignment to fail under `-Werror`
        // when `char` is signed.
        if self.foldable(e) {
            match const_eval::eval_with(e, &|x| self.sema.const_at(x.span)) {
                Ok(const_eval::Const::Int(v)) => return format!("((int32_t){}u)", v),
                Ok(const_eval::Const::Bool(b)) => {
                    return if b { "1".to_string() } else { "0".to_string() }
                }
                _ => {}
            }
        }
        match &e.kind {
            ExprKind::Int(v) => format!("((int32_t){}u)", v),
            ExprKind::Real(v) => format!("((float){v:?}f)"),
            ExprKind::Char(c) => format!("((char){}u)", c),
            ExprKind::Bool(b) => if *b { "1".into() } else { "0".into() },
            ExprKind::Nil => "NULL".into(),
            ExprKind::Str(text) => {
                // A one-character string in a CHAR context: report 10.1
                // writes `ch <= "9"` and `ORD("0")`.
                if let Some(w) = want {
                    if self.types.is_char(w) {
                        return format!("\"{}\"[0]", escape_c(text));
                    }
                }
                format!("\"{}\"", escape_c(text))
            }
            ExprKind::Designator(d) => {
                if d.sels.is_empty() {
                    if let Some(const_eval::Const::Str(text)) = self.sema.const_at(e.span) {
                        if want.is_some_and(|t| self.types.is_char(t)) {
                            return format!("\"{}\"[0]", escape_c(&text));
                        }
                        return format!("\"{}\"", escape_c(&text));
                    }
                }
                if matches!(d.sels.last(), Some(Selector::Call(_))) {
                    return self.call_value(d, self.current_level());
                }
                self.designator(d, self.current_level(), false)
            }
            ExprKind::Unary(op, x) => {
                let ty = self.sema.type_at(e.span, self.types).unwrap_or(TypeId(0));
                let v = self.expr(x, Some(ty));
                match op {
                    UnOp::Not => format!("(!({v}))"),
                    UnOp::Neg if self.types.is_set(ty) => format!("(~({v}))"),
                    UnOp::Neg => format!("(-({v}))"),
                    UnOp::Plus => format!("(+({v}))"),
                }
            }
            ExprKind::TypeTest(x, q) => {
                let v = self.expr(x, None);
                let ty = self.sema.type_at(x.span, self.types).unwrap_or(TypeId(0));
                let t = match self.qident_type(q) {
                    Some(t) => t,
                    None => {
                        self.unresolved += 1;
                        TypeId(0)
                    }
                };
                let target = self.descriptor_name(t);
                let test = format!("rt_is({}, {target})", self.dyn_of(&v, ty));
                // `NIL IS T` is false. Since NIL's descriptor address is zero,
                // test for NIL first (W9.2b, found when comparing the back ends).
                if self.types.is_pointer(ty) {
                    format!("(({v}) != NULL && {test})")
                } else {
                    test
                }
            }
            ExprKind::Set(elems) => {
                if elems.is_empty() {
                    return "0u".into();
                }
                let mut parts = Vec::new();
                for el in elems {
                    let lo = self.expr(&el.lo, Some(self.types.integer()));
                    match &el.hi {
                        Some(h) => {
                            let hi = self.expr(h, Some(self.types.integer()));
                            parts.push(format!("rt_set({lo}, {hi})"));
                        }
                        None => parts.push(format!("rt_set({lo}, {lo})")),
                    }
                }
                format!("({})", parts.join(" | "))
            }
            ExprKind::Binary(op, l, r) => self.binary(*op, l, r),
        }
    }

    /// The descriptor symbol for a type. Pointer types have no descriptor of
    /// their own, so use the base record's symbol (DESIGN 9.3).
    fn descriptor_name(&self, t: TypeId) -> String {
        let rec = if self.types.is_pointer(t) {
            self.types.deref(t).unwrap_or(t)
        } else {
            t
        };
        match self.type_c_name(rec) {
            Some(n) => format!("&TD_{n}"),
            None => "NULL".into(),
        }
    }

    fn binary(&mut self, op: BinOp, l: &Expr, r: &Expr) -> String {
        let lt = self.sema.type_at(l.span, self.types).unwrap_or(TypeId(0));
        let rt = self.sema.type_at(r.span, self.types).unwrap_or(TypeId(0));
        // Identical side-effect-free scalar operands compare equal (or ordered)
        // except for REAL, where NaN affects the result. Folding also avoids C
        // warnings for tautological comparisons (R20). Calls aren't folded
        // because they can have side effects or return different values.
        if self.types.same_type(lt, rt)
            && !matches!(self.types.get(lt), Type::Basic(Basic::Real))
            && matches!(op, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge)
            && self.same_plain(l, r)
        {
            let truth = matches!(op, BinOp::Eq | BinOp::Le | BinOp::Ge);
            return if truth { "1".into() } else { "0".into() };
        }
        // A one-character string on either side makes this a CHAR comparison.
        let want = if self.types.is_char(lt) {
            Some(lt)
        } else if self.types.is_char(rt) {
            Some(rt)
        } else {
            None
        };
        // C sequences `&&` and `||`. Sequencing them by hand would eagerly
        // evaluate the right operand, defeating short-circuiting.
        let sequenced = !matches!(op, BinOp::And | BinOp::Or)
            && (self.has_call(l) || self.has_call(r));
        let array_of_operands = {
            let char_cmp = self.types.is_char_like(lt) && self.types.is_char_like(rt);
            !char_cmp
                && ((self.types.is_char_array(lt) && !self.types.is_char_like(lt))
                    || (self.types.is_char_array(rt) && !self.types.is_char_like(rt)))
        };
        let mut a = self.expr(l, want);
        // An `ADR` operand has no side effect and is as wide as the host's
        // pointers (R31), so it is neither sequenced nor narrowed: a temporary of
        // its Oberon type would cut the address in half, and the comparison below
        // would then compare two truncations.
        if sequenced && !self.is_address(l) && !array_of_operands && !self.is_aggregate(lt) {
            a = self.materialise(want.unwrap_or(lt), &a);
        }
        let mut b = if matches!(op, BinOp::And | BinOp::Or) {
            self.in_conditional += 1;
            let b = self.expr(r, want);
            self.in_conditional -= 1;
            b
        } else {
            self.expr(r, want)
        };
        if sequenced && !self.is_address(r) && !array_of_operands && !self.is_aggregate(rt) {
            b = self.materialise(want.unwrap_or(rt), &b);
        }
        // Single-character strings compare as ordinals. C `char` may be signed,
        // which would reverse `255X > 1X` (report 8.2.4). Longer strings are
        // arrays and use the bounded comparison below.
        let char_cmp = self.types.is_char_like(lt) && self.types.is_char_like(rt);
        if char_cmp {
            a = format!("(unsigned char)({a})");
            b = format!("(unsigned char)({b})");
        }
        let array_cmp = !char_cmp
            && ((self.types.is_char_array(lt) && !self.types.is_char_like(lt))
                || (self.types.is_char_array(rt) && !self.types.is_char_like(rt)));
        if matches!(op, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge)
            && array_cmp
        {
            // Bounded comparison supports CHAR arrays without a terminator
            // (DESIGN 9.5, risk R14).
            let (na, nb) = (self.char_len(lt), self.char_len(rt));
            if sequenced {
                a = self.materialise_addr("const char", &format!("(const char *)&({a})"));
                b = self.materialise_addr("const char", &format!("(const char *)&({b})"));
            }
            // With fixed addresses, the temporaries are the arguments. Otherwise
            // the addresses are taken in place.
            let cmp = if sequenced {
                format!("rt_charcmp({a}, {na}, {b}, {nb})")
            } else {
                format!("rt_charcmp((const char *)&({a}), {na}, (const char *)&({b}), {nb})")
            };
            return match op {
                BinOp::Eq => format!("({cmp} == 0)"),
                BinOp::Ne => format!("({cmp} != 0)"),
                BinOp::Lt => format!("({cmp} < 0)"),
                BinOp::Le => format!("({cmp} <= 0)"),
                BinOp::Gt => format!("({cmp} > 0)"),
                _ => format!("({cmp} >= 0)"),
            };
        }
        // Report 8.2.3 defines the SET operators `+ - * /` as union, difference,
        // intersection, and symmetric difference. Unary `-` means complement.
        if self.types.is_set(lt) && self.types.is_set(rt) {
            return match op {
                BinOp::Add => format!("({a} | {b})"),
                BinOp::Sub => format!("({a} & ~{b})"),
                BinOp::Mul => format!("({a} & {b})"),
                BinOp::Div => format!("({a} ^ {b})"),
                _ => self.binary_plain(op, &a, &b, lt, rt, l.span.lo),
            };
        }
        self.binary_plain(op, &a, &b, lt, rt, l.span.lo)
    }

    /// Whether this expression can call something. Calls are the only expression
    /// side effect. Classify every form so new expressions aren't assumed safe.
    fn has_call(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Designator(d) => d.sels.iter().any(|s| match s {
                Selector::Call(_) => true,
                Selector::Index(ix) => ix.iter().any(|x| self.has_call(x)),
                _ => false,
            }),
            ExprKind::Unary(_, x) => self.has_call(x),
            ExprKind::Binary(_, l, r) => self.has_call(l) || self.has_call(r),
            ExprKind::Set(elems) => elems
                .iter()
                .any(|el| self.has_call(&el.lo) || el.hi.as_ref().is_some_and(|h| self.has_call(h))),
            ExprKind::TypeTest(x, _) => self.has_call(x),
            _ => false,
        }
    }

    /// True when an expression is built only from literals, selector-free
    /// designators and operators: folding such a tree cannot skip a call, and no
    /// part of it can have a side effect. Exhaustive on purpose, so a new
    /// expression form has to be classified rather than defaulting to "safe".
    fn foldable(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Int(_)
            | ExprKind::Real(_)
            | ExprKind::Char(_)
            | ExprKind::Str(_)
            | ExprKind::Bool(_) => true,
            // A set's bounds are expressions, and `1 IN {ABS(1)}` is exactly the
            // shape the call check exists for.
            ExprKind::Set(elems) => elems.iter().all(|el| {
                self.foldable(&el.lo) && el.hi.as_ref().is_none_or(|h| self.foldable(h))
            }),
            ExprKind::Designator(d) => d.sels.is_empty(),
            ExprKind::Unary(_, x) => self.foldable(x),
            ExprKind::Binary(_, l, r) => self.foldable(l) && self.foldable(r),
            ExprKind::Nil | ExprKind::TypeTest(..) => false,
        }
    }

    /// Whether two operands have the same literal or selector-free designator.
    /// Derived `PartialEq` includes source spans, so separate occurrences of
    /// `x` in `x = x` do not compare equal. Compare only the relevant structure.
    /// Identical expressions without calls or selectors cannot differ between
    /// evaluations, so `x > x` folds to a constant. A call may return a
    /// different value or have side effects. A selector can trap on a
    /// dereference, index, or type guard, so folding it would omit the trap.
    fn same_plain(&self, l: &Expr, r: &Expr) -> bool {
        !self.has_call(l) && self.trap_free(l) && self.same_tree(l, r)
    }

    /// Structural equality that ignores spans: parsed occurrences of the same
    /// designator have different spans.
    fn same_tree(&self, l: &Expr, r: &Expr) -> bool {
        match (&l.kind, &r.kind) {
            (ExprKind::Int(a), ExprKind::Int(b)) => a == b,
            (ExprKind::Real(a), ExprKind::Real(b)) => a.to_bits() == b.to_bits(),
            (ExprKind::Char(a), ExprKind::Char(b)) => a == b,
            (ExprKind::Str(a), ExprKind::Str(b)) => a == b,
            (ExprKind::Bool(a), ExprKind::Bool(b)) => a == b,
            (ExprKind::Nil, ExprKind::Nil) => true,
            // A constant set is equal when the folder gives the same value; its
            // elements are expressions with spans, so it cannot be compared
            // structurally like the rest.
            (ExprKind::Set(_), ExprKind::Set(_)) => {
                self.foldable(l)
                    && matches!(
                        (const_eval::eval(l), const_eval::eval(r)),
                        (Ok(x), Ok(y)) if x == y
                    )
            }
            (ExprKind::Designator(a), ExprKind::Designator(b)) => {
                a.base == b.base && self.same_sels(&a.sels, &b.sels)
            }
            (ExprKind::Unary(op, x), ExprKind::Unary(po, y)) => op == po && self.same_tree(x, y),
            (ExprKind::Binary(op, a, b), ExprKind::Binary(po, c, d)) => {
                // `a + b >= b + a` is a comparison of a value with itself too,
                // and gcc proves it and rejects the C for it (R20). Only the
                // operators that commute are reordered: not `/`, `DIV`, `MOD`
                // or `-`, and not a set difference.
                op == po
                    && ((self.same_tree(a, c) && self.same_tree(b, d))
                        || (matches!(op, BinOp::Add | BinOp::Mul | BinOp::And | BinOp::Or)
                            && self.same_tree(a, d)
                            && self.same_tree(b, c)))
            }
            (ExprKind::TypeTest(x, t), ExprKind::TypeTest(y, u)) => t == u && self.same_tree(x, y),
            _ => false,
        }
    }

    fn same_sels(&self, l: &[Selector], r: &[Selector]) -> bool {
        l.len() == r.len()
            && l.iter().zip(r).all(|(a, b)| match (a, b) {
                (Selector::Field(x), Selector::Field(y)) => x == y,
                (Selector::Deref, Selector::Deref) => true,
                (Selector::Guard(x), Selector::Guard(y)) => x == y,
                (Selector::Index(x), Selector::Index(y)) => self.same_args(x, y),
                (Selector::Call(x), Selector::Call(y)) => self.same_args(x, y),
                _ => false,
            })
    }

    fn same_args(&self, l: &[Expr], r: &[Expr]) -> bool {
        l.len() == r.len() && l.iter().zip(r).all(|(a, b)| self.same_tree(a, b))
    }

    /// Side-effect-free and trap-free: a literal, a selector-free designator, a
    /// constant set, or those combined by an operator. Structural equality of
    /// two such trees may be folded because both sides give the same value.
    fn trap_free(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Int(_)
            | ExprKind::Real(_)
            | ExprKind::Char(_)
            | ExprKind::Str(_)
            | ExprKind::Bool(_)
            | ExprKind::Nil => true,
            ExprKind::Designator(d) => d.sels.is_empty(),
            ExprKind::Set(_) => self.foldable(e),
            ExprKind::Unary(_, x) => self.trap_free(x),
            ExprKind::Binary(_, a, b) => self.trap_free(a) && self.trap_free(b),
            ExprKind::TypeTest(x, _) => self.trap_free(x),
        }
    }

    /// `at` is the byte offset of the expression, which a division by zero
    /// reports: every trap in the emitted code is locatable (DESIGN 9.4).
    fn binary_plain(&self, op: BinOp, a: &str, b: &str, lt: TypeId, rt: TypeId, at: u32) -> String {
        match op {
            BinOp::Or => format!("({a} || {b})"),
            BinOp::And => format!("({a} && {b})"),
            BinOp::Add => format!("({a} + {b})"),
            BinOp::Sub => format!("({a} - {b})"),
            BinOp::Mul => format!("({a} * {b})"),
            // For integers, `/` uses the same quotient as DIV (report 8.2.2).
            // REAL division and SET symmetric difference are not affected.
            BinOp::Div if self.types.is_integer_like(lt) && self.types.is_integer_like(rt) => {
                format!("rt_div_at({a}, {b}, {at})")
            }
            BinOp::Div => format!("({a} / {b})"),
            BinOp::IDiv => format!("rt_div_at({a}, {b}, {at})"),
            BinOp::Mod => format!("rt_mod_at({a}, {b}, {at})"),
            BinOp::In => format!("rt_in({b}, {a})"),
            BinOp::Eq => format!("({a} == {b})"),
            BinOp::Ne => format!("({a} != {b})"),
            BinOp::Lt => format!("({a} < {b})"),
            BinOp::Le => format!("({a} <= {b})"),
            BinOp::Gt => format!("({a} > {b})"),
            BinOp::Ge => format!("({a} >= {b})"),
        }
    }

    fn char_len(&self, t: TypeId) -> u32 {
        match self.types.get(t) {
            Type::Array { len, .. } => *len,
            Type::StringLit(n) => *n as u32,
            _ => 0,
        }
    }

    fn current_level(&self) -> usize {
        self.frames.last().map(|f| f.level).unwrap_or(1)
    }

    /// How to write a symbol at its use: a plain name or a frame field reached
    /// through static links. A `VAR` parameter instead needs a dereference.
    /// The hidden length for an open-array formal follows the same access path,
    /// including static links for an enclosing procedure's parameter.
    fn access_len(&self, s: &Sym, level: usize) -> String {
        let place = if s.level == 0 || s.level >= level {
            s.cname.clone()
        } else {
            let mut link = String::from("sl");
            for _ in 0..(level - s.level - 1) {
                link = format!("(*(void **)({link}))");
            }
            let frame = self.frame_at(s.level);
            format!("(({frame} *){link})->{}", s.cname.trim_start_matches("f."))
        };
        format!("{place}__len")
    }

    fn access(&self, s: &Sym, level: usize) -> String {
        let place = if s.level == 0 || s.level >= level {
            s.cname.clone()
        } else {
            // Walk the static links (oberon/csmoke's `nested-link-*` scheme).
            // Every frame keeps its own link in its first word.
            // `sl` points at the parent's frame. A symbol one level up needs
            // zero hops; one two levels up needs one, and so on.
            let mut link = String::from("sl");
            for _ in 0..(level - s.level - 1) {
                link = format!("(*(void **)({link}))");
            }
            let frame = self.frame_at(s.level);
            format!("(({frame} *){link})->{}", s.cname.trim_start_matches("f."))
        };
        // A `VAR` parameter points to the actual, so access dereferences it.
        // Open arrays are the exception: their parameter already points to the
        // first element (section 9.2). Dereferencing would produce `(*v)[i]`,
        // which does not compile (PLAN.md W7.4).
        if s.by_var && !matches!(self.types.get(s.ty), Type::OpenArray { .. }) {
            format!("(*{place})")
        } else {
            place
        }
    }

    fn frame_at(&self, level: usize) -> String {
        self.frames
            .iter()
            .find(|f| f.level == level)
            .map(|f| f.frame.clone())
            .unwrap_or_default()
    }

    /// Where a field is stored: derived records embed their base as the first
    /// member (DESIGN 9.3), so inherited fields use `.base.base.field`.
    fn field_path(&self, rec: TypeId, f: &str) -> Option<String> {
        let mut cur = rec;
        let mut prefix = String::new();
        for _ in 0..64 {
            match self.types.get(cur) {
                Type::Record { base, fields, .. } => {
                    if fields.iter().any(|field| field.name == *f) {
                        return Some(prefix);
                    }
                    prefix.push_str("base.");
                    cur = (*base)?;
                }
                _ => return None,
            }
        }
        None
    }

    /// The dotted path from a record to the descriptor field, which lives in
    /// the base-most record of the chain ("" when the record is base-less).
    fn desc_field_path(&self, t: TypeId) -> String {
        let mut path = String::new();
        let mut cur = match self.types.get(t) {
            Type::Record { base, .. } => *base,
            _ => None,
        };
        while let Some(b) = cur {
            path.push_str("base.");
            cur = match self.types.get(b) {
                Type::Record { base, .. } => *base,
                _ => None,
            };
        }
        path
    }

    /// The pointer map of a record type: where the pointers are inside a region
    /// `offsetof` terms from the C layout, which the back end does not compute.
    /// The map uses the same walk as `desc_init` to describe nested records.
    /// Since a base's fields are also derived-record fields, each level adds an
    /// `offsetof` term.
    fn record_fields(&mut self, t: TypeId) -> Vec<String> {
        let mut out = Vec::new();
        self.collect_record_fields(t, "", &mut out);
        out
    }

    fn collect_record_fields(&mut self, t: TypeId, prefix: &str, out: &mut Vec<String>) {
        let (base, fields, here) = match self.types.get(t) {
            Type::Record { base, fields, .. } => {
                (*base, fields.clone(), self.type_c_name(t).unwrap_or_default())
            }
            _ => return,
        };
        if let Some(b) = base {
            self.collect_record_fields(b, &format!("{prefix}offsetof({here}, base) + "), out);
        }
        for f in &fields {
            let off = format!("{prefix}offsetof({here}, {})", mangle(&f.name));
            out.extend(self.region_fields(f.ty, &off));
        }
    }

    /// Map entries for a region of type `t` beginning at `off` (DESIGN 15.2).
    /// A pointer field is one slot; arrays of pointers are runs of slots, and
    /// arrays of records are runs of descriptor-fronted records. A record field
    /// is one run. Integers and procedure variables contain no object pointers.
    /// A record without pointers has no map, so marking it costs one NULL test.
    fn region_fields(&mut self, t: TypeId, off: &str) -> Vec<String> {
        match self.types.get(t).clone() {
            Type::Pointer { .. } => vec![format!("{off}, 0, 1, NULL")],
            Type::Array { .. } => {
                let (n, elem) = self.array_run(t);
                if !self.reaches_pointer(elem) {
                    return Vec::new();
                }
                match self.types.get(elem).clone() {
                    Type::Pointer { .. } => vec![format!("{off}, sizeof(void *), {n}, NULL")],
                    Type::Record { .. } => vec![format!(
                        "{off}, sizeof({}), {n}, {}",
                        self.c_type(elem),
                        self.descriptor_name(elem)
                    )],
                    _ => Vec::new(),
                }
            }
            Type::Record { .. } => {
                if !self.reaches_pointer(t) {
                    return Vec::new();
                }
                vec![format!("{off}, 0, 1, {}", self.descriptor_name(t))]
            }
            _ => Vec::new(),
        }
    }

    /// Flatten an array type to its innermost element and element count.
    /// Dimensions are contiguous in the C struct, so one run uses the element
    /// size across all source dimensions.
    fn array_run(&self, t: TypeId) -> (usize, TypeId) {
        // Saturate the product. An array with that many elements cannot fit in
        // machine storage. A wrapped count would describe the wrong run.
        let mut n: usize = 1;
        let mut elem = t;
        while let Type::Array { len, elem: e } = self.types.get(elem).clone() {
            n = n.saturating_mul(len as usize);
            elem = e;
        }
        (n, elem)
    }

    /// exponential in its depth; records cannot contain themselves by value, so
    /// the walk ends at a pointer back to the record.
    fn reaches_pointer(&mut self, t: TypeId) -> bool {
        if let Some(&known) = self.reaches.get(&t) {
            return known;
        }
        let found = match self.types.get(t).clone() {
            Type::Pointer { .. } => true,
            Type::Array { .. } => {
                let (_, elem) = self.array_run(t);
                self.reaches_pointer(elem)
            }
            Type::Record { base, fields, .. } => {
                let mut any = base.map(|b| self.reaches_pointer(b)).unwrap_or(false);
                for f in &fields {
                    any = any || self.reaches_pointer(f.ty);
                }
                any
            }
            _ => false,
        };
        self.reaches.insert(t, found);
        found
    }

    /// Does a type contain a record anywhere (so its descriptor fields need
    /// initializing)?
    fn contains_record(&self, t: TypeId) -> bool {
        match self.types.get(t) {
            Type::Record { .. } => true,
            Type::Array { elem, .. } => self.contains_record(*elem),
            Type::OpenArray { .. } => false,
            _ => false,
        }
    }

    /// Give a record storage its static descriptor and every record *inside* it
    /// its own descriptor (DESIGN 9.3). The outermost record owns the descriptor
    /// word, so only this function sets it. It visits base fields for their
    /// descriptors, not the base descriptor word.
    fn desc_init(&self, t: TypeId, expr: &str, out: &mut String, indent: &str) {
        match self.types.get(t).clone() {
            Type::Record { base, fields, .. } => {
                let path = self.desc_field_path(t);
                let name = match self.type_c_name(t) {
                    Some(n) => n,
                    None => return,
                };
                let _ = writeln!(out, "{indent}{expr}.{path}desc = &TD_{name};");
                self.desc_init_fields(base, &fields, expr, out, indent);
            }
            Type::Array { len, elem } if self.contains_record(elem) => {
                let _ = writeln!(out, "{indent}{{ int32_t i__;");
                let _ = writeln!(out, "{indent}for (i__ = 0; i__ < {len}; i__++) {{");
                self.desc_init(elem, &format!("{expr}.e[i__]"), out, &format!("{indent}    "));
                let _ = writeln!(out, "{indent}}} }}");
            }
            _ => {}
        }
    }

    /// Visit the record-valued fields of a record type and of its base types,
    /// without touching any descriptor word.
    fn desc_init_fields(
        &self,
        base: Option<TypeId>,
        fields: &[Field],
        expr: &str,
        out: &mut String,
        indent: &str,
    ) {
        for field in fields {
            if self.contains_record(field.ty) {
                self.desc_init(field.ty, &format!("{expr}.{}", mangle(&field.name)), out, indent);
            }
        }
        if let Some(b) = base {
            if let Type::Record {
                base: bb,
                fields: bf,
                ..
            } = self.types.get(b).clone()
            {
                self.desc_init_fields(bb, &bf, &format!("{expr}.base"), out, indent);
            }
        }
    }

    /// `lvalue` renders a designator where it has to be assignable: a narrowed
    /// name keeps its plain spelling (a cast is not an lvalue) and a guard's
    /// check becomes a statement in the prelude, which every lvalue caller
    /// flushes before it writes (DESIGN R29).
    fn designator(&mut self, d: &ast::Designator, level: usize, lvalue: bool) -> String {
        let base_sym = self.lookup(&d.base[0]);
        // A module-qualified base resolves to the other module's C name; its
        // remaining selectors apply exactly as for a local one (`M.r.f`).
        let qualified = self.qualified_cname_for(&d.base);
        let mut ty = match (&qualified, self.sema.type_at(d.span, self.types)) {
            (Some(_), Some(t)) => t,
            _ => base_sym.as_ref().map(|s| s.ty).unwrap_or(TypeId(0)),
        };
        // A narrowed name rendered as an lvalue uses its plain spelling, because
        // the cast that gives a *field* its layout is not itself an lvalue. It is
        // only usable when the name is the whole designator: with a selector
        // after it the cast has to stay, since the field offset belongs to the
        // narrowed record (DESIGN R29).
        let plain_base =
            lvalue && d.sels.is_empty() && base_sym.as_ref().and_then(|s| s.plain.as_ref()).is_some();
        let mut text = match (&qualified, &base_sym) {
            (Some(c), _) => c.clone(),
            (None, Some(s)) if plain_base => {
                s.plain.clone().unwrap_or_else(|| self.access(s, level))
            }
            (None, Some(s)) => self.access(s, level),
            (None, None) => mangle(&d.base[0]),
        };
        let mut is_pointer = self.types.is_pointer(ty);
        // `rt_nil(p, at)` reads `p` twice (that is what makes it an lvalue-
        // preserving expression rather than a hoisted statement), so it is only
        // sound while `p` has no side effect. An *index* expression may contain a
        // call, so the flag is tracked selector by selector.
        let mut volatile = false;
        // Open-array formals have a hidden length (9.2), which the index check
        // needs. Only a designator's base can be open, so capture it once here.
        let mut open_len: Option<String> = base_sym
            .as_ref()
            .filter(|s| matches!(self.types.get(s.ty), Type::OpenArray { .. }))
            .map(|s| self.access_len(s, level));
        for (si, sel) in d.sels.iter().enumerate() {
            // The last selector is the one whose view may have to be assignable.
            let last = si + 1 == d.sels.len();
            match sel {
                Selector::Field(f) => {
                    let rec = if is_pointer {
                        self.types.deref(ty).unwrap_or(ty)
                    } else {
                        ty
                    };
                    let path = self.field_path(rec, f).unwrap_or_default();
                    if is_pointer && !volatile && self.checks.nil {
                        // The check is part of the access, so a NIL target traps
                        // with this designator's offset (R21).
                        text = format!("rt_nil({text}, {})", d.span.lo);
                    }
                    let sep = if is_pointer { "->" } else { "." };
                    let _ = write!(text, "{sep}{path}{}", mangle(f));
                    ty = self.types.field(rec, f).unwrap_or(TypeId(0));
                    // The field may itself be a pointer (`p.next.value`).
                    is_pointer = self.types.is_pointer(ty);
                }
                Selector::Index(ix) => {
                    if ix.iter().any(|e| self.has_call(e)) {
                        volatile = true;
                    }
                    let mut open = matches!(self.types.get(ty), Type::OpenArray { .. });
                    for e in ix {
                        let i = self.expr(e, Some(self.types.integer()));
                        if open {
                            // Open-array parameters are pointers with a hidden
                            // length (report 10.1). Use it for bounds checks;
                            // the trap matrix found this omission (PLAN.md W7.4).
                            match open_len.take() {
                                Some(len) if self.checks.index => {
                                    let _ = write!(text, "[rt_index({i}, {len}, {})]", d.span.lo);
                                }
                                _ => {
                                    let _ = write!(text, "[{i}]");
                                }
                            }
                        } else {
                            match self.types.array_len(ty) {
                                Some(n) if self.checks.index => {
                                    let _ = write!(text, ".e[rt_index({i}, {n}, {})]", d.span.lo);
                                }
                                _ => {
                                    let _ = write!(text, ".e[{i}]");
                                }
                            }
                        }
                        // Advance one array level for each index (report 8.1).
                        ty = self.types.elem(ty).unwrap_or(TypeId(0));
                        // An element may be a pointer, and a further dimension
                        // may be an open array.
                        is_pointer = self.types.is_pointer(ty);
                        open = matches!(self.types.get(ty), Type::OpenArray { .. });
                    }
                }
                Selector::Deref => {
                    // Same rule for `p^`: the check is inside the dereference.
                    if is_pointer && !volatile && self.checks.nil {
                        text = format!("rt_nil({text}, {})", d.span.lo);
                    }
                    text = format!("(*({text}))");
                    ty = self.types.deref(ty).unwrap_or(TypeId(0));
                    is_pointer = self.types.is_pointer(ty);
                }
                Selector::Guard(q) => {
                    // A guard `v(T0)` asserts that `v` has type T0 and aborts if
                    // not (report 8.1). Keep the check in the expression so it
                    // stays inside short-circuit operands and rejects NIL before
                    // reading its descriptor.
                    let t = match self.qident_type(q) {
                        Some(t) => t,
                        None => {
                            self.unresolved += 1;
                            TypeId(0)
                        }
                    };
                    if self.types.is_pointer(t) {
                        let rec = self.types.deref(t).unwrap_or(t);
                        let desc = self.descriptor_name(rec);
                        let dyn_test = format!(
                            "(({text}) != NULL && rt_is((*(const ObDesc *const *)(const void *)({text})), {desc}))"
                        );
                        let view = format!("(({}){text})", self.c_type(t));
                        let test = format!("rt_guard_is({dyn_test}, \"type guard failed at byte {}\")", d.span.lo);
                        text = if lvalue {
                            // A target must be an lvalue, but a comma expression
                            // isn't one. Emit the check as a statement before
                            // writes through the target. Lvalue callers flush it.
                            // Keep the cast for field access. Its offset uses
                            // the narrowed record (DESIGN R29).
                            if self.checks.guard {
                                let _ = writeln!(self.prelude, "    {test};");
                            }
                            if last {
                                text.clone()
                            } else {
                                view
                            }
                        } else if self.checks.guard {
                            format!("(({test}, {view}))")
                        } else {
                            view
                        };
                        ty = t;
                        is_pointer = true;
                    } else {
                        let sub = if self.types.is_pointer(ty) {
                            format!("(*({text}))")
                        } else {
                            text.clone()
                        };
                        let sub_ty = if self.types.is_pointer(ty) {
                            self.types.deref(ty).unwrap_or(ty)
                        } else {
                            ty
                        };
                        let desc = self.descriptor_name(t);
                        let dyn_test = self.dyn_of(&sub, sub_ty);
                        let view = format!("(*(({} *)(void *)&({sub})))", self.c_type(t));
                        let test = format!(
                            "rt_guard_is(rt_is({dyn_test}, {desc}), \"type guard failed at byte {}\")",
                            d.span.lo
                        );
                        text = if lvalue {
                            if self.checks.guard {
                                let _ = writeln!(self.prelude, "    {test};");
                            }
                            if last {
                                sub
                            } else {
                                view
                            }
                        } else if self.checks.guard {
                            format!("(({test}, {view}))")
                        } else {
                            view
                        };
                        ty = t;
                        is_pointer = false;
                    }
                }
                Selector::Call(_) | Selector::Args(_) => {}
            }
        }
        text
    }

    // Calls

    /// A call used as a statement.
    fn call(&mut self, d: &ast::Designator, level: usize, out: &mut String) {
        let name = d.base[0].clone();
        if let Some(entry) = self.system_entry(&d.base) {
            if SYSTEM_PROPER.contains(&entry) {
                self.sys_proper(entry, d, level, out);
                return;
            }
        }
        if self.lookup(&name).is_none() && PREDEFINED_PROPER.contains(&name.as_str()) {
            self.predef_proper(&name, d, level, out);
            return;
        }
        let (callee, actuals) = self.call_parts(d, level, out);
        let _ = writeln!(out, "    {callee}({actuals});");
    }

    /// A call used for its value. Temporaries go to the prelude, which the
    /// enclosing statement flushes where the expression is evaluated once.
    fn call_value(&mut self, d: &ast::Designator, level: usize) -> String {
        let name = d.base[0].clone();
        let args = match d.sels.last() {
            Some(Selector::Call(a)) => a.clone(),
            _ => Vec::new(),
        };
        // `MIN(T)`/`MAX(T)` (W8.2) use a constant recorded by the checker.
        // A user's own `MIN` records nothing, so it doesn't need a check here.
        if let Some(v) = self.sema.extreme_at(d.span) {
            return format!("((int32_t){}u)", v as u32);
        }
        if let Some(entry) = self.system_entry(&d.base) {
            if SYSTEM_FUNCTIONS.contains(&entry) {
                return self.sys_function(entry, &args, level);
            }
        }
        if self.lookup(&name).is_none() && PREDEFINED_FUNCTIONS.contains(&name.as_str()) {
            return self.predef_function(&name, &args, level);
        }
        let mut sink = String::new();
        let (callee, actuals) = self.call_parts(d, level, &mut sink);
        self.prelude.push_str(&sink);
        format!("{callee}({actuals})")
    }

    /// The callee and arguments, adjusted for parameter modes: nested procedures
    /// take a static link, and open arrays take a pointer and length (DESIGN 9.7).
    fn call_parts(&mut self, d: &ast::Designator, level: usize, out: &mut String) -> (String, String) {
        let name = d.base[0].clone();
        let args = match d.sels.last() {
            Some(Selector::Call(a)) => a.clone(),
            _ => Vec::new(),
        };
        // The callee omits the argument list. It may be a bare name, a
        // module-qualified name, or a procedure-valued field or element
        // (`r.f(1)`, `a[i](1)`) allowed by the report (10.1).
        // A bare name may represent a parameterless call. Other selectors yield
        // a procedure value.
        let plain = d.sels.is_empty() || matches!(d.sels.as_slice(), [Selector::Call(_)]);
        let (callee, callee_ty, level_of_callee, is_declared_proc) = if let Some(c) = self
            .qualified_cname_for(&d.base)
        {
            let ty = self
                .sema
                .call_types
                .get(&sema::sema::span_key(d.span))
                .copied()
                .unwrap_or(TypeId(0));
            (c, ty, 1usize, false)
        } else if plain {
            match self.lookup(&name) {
                Some(sym) => (sym.cname, sym.ty, sym.level, sym.decl_proc),
                None => (mangle(&name), TypeId(0), level, false),
            }
        } else {
            // Emit a selected procedure value as a designator.
            let head = ast::Designator {
                base: d.base.clone(),
                sels: d.sels[..d.sels.len() - 1].to_vec(),
                span: d.span,
            };
            let ty = self.sema.type_at(d.span, self.types).unwrap_or(TypeId(0));
            (self.designator(&head, level, false), ty, level, false)
        };
        let callee_ty = match self.sema.call_types.get(&sema::sema::span_key(d.span)) {
            Some(t) => *t,
            None => callee_ty,
        };
        let params = match self.types.get(callee_ty) {
            Type::Proc { params, .. } => params.clone(),
            _ => Vec::new(),
        };
        let mut actuals: Vec<String> = Vec::new();
        if is_declared_proc && level_of_callee >= 2 {
            // Calling a child passes our own frame; calling a sibling or an
            // uncle passes the frame of the callee's parent, reached by links.
            let link = if level_of_callee == level + 1 {
                "&f".to_string()
            } else {
                let mut l = String::from("sl");
                for _ in 0..(level - level_of_callee) {
                    l = format!("(*(void **)({l}))");
                }
                l
            };
            actuals.push(link);
        }
        // C leaves call argument order unspecified. A callee may write a variable
        // that another argument reads, as in `F0(ABS(F0(100, acc)),
        // LSL(ASR(m0, 0), 1))`. If any argument has side effects, fix value
        // arguments in source order. Address arguments must remain addresses.
        let seq = args.iter().any(|a| self.has_call(a));
        for (i, a) in args.iter().enumerate() {
            let formal = params
                .get(i)
                .cloned()
                .unwrap_or(Param { by_var: false, ty: TypeId(0) });
            if let Type::OpenArray { elem } = self.types.get(formal.ty) {
                let (ptr, len) = self.open_array_actual(a, *elem, level);
                actuals.push(ptr);
                actuals.push(len);
                continue;
            }
            if formal.by_var {
                let v = self.designator_of(a, level);
                // A record formal may be a base of the actual type (report 10.1).
                // Convert to its embedded base in C (DESIGN 9.3).
                let got = self.sema.type_at(a.span, self.types).unwrap_or(TypeId(0));
                if self.types.is_record(formal.ty)
                    && self.types.is_record(got)
                    && !self.types.same_type(formal.ty, got)
                {
                    actuals.push(format!("({} *)&({v})", self.c_type(formal.ty)));
                } else {
                    actuals.push(format!("&({v})"));
                }
                continue;
            }
            if self.types.is_record(formal.ty) {
                // The caller passes a copy by pointer (report 10.1, DESIGN 9.7).
                // The copy keeps the formal's descriptor rather than the actual's.
                //
                // Inside a `&&`/`||` operand, make the copy only when that operand
                // is evaluated. Hoist the declaration, then form a comma
                // expression whose value is the copy's address (R16, PLAN.md
                // W2.3). This also preserves short-circuiting in a selector of
                // the actual argument.
                self.anon += 1;
                let tmp = format!("tmp__{}", self.anon);
                let decl = self.binding(formal.ty, &tmp);
                let src = self.designator_of(a, level);
                let desc = self.descriptor_name(formal.ty);
                if self.in_conditional > 0 {
                    let _ = writeln!(self.prelude, "    {decl};");
                    actuals.push(format!(
                        "((rt_rec_copy(&{tmp}, &({src}), (int32_t)sizeof({tmp}), {desc}), &{tmp}))"
                    ));
                } else {
                    let _ = writeln!(out, "    {decl};");
                    let _ = writeln!(
                        out,
                        "    rt_rec_copy(&{tmp}, &({src}), (int32_t)sizeof({tmp}), {desc});"
                    );
                    actuals.push(format!("&{tmp}"));
                }
                continue;
            }
            let got = self.sema.type_at(a.span, self.types);
            // Use the formal type for a one-character string passed to CHAR
            // (report 9.1.2); otherwise use the actual type.
            let want = if self.types.is_char(formal.ty) {
                Some(formal.ty)
            } else {
                got.or(Some(formal.ty))
            };
            let mut text = self.expr(a, want);
            if seq {
                match got {
                    Some(t) if !self.is_aggregate(t) => text = self.materialise(t, &text),
                    _ => {}
                }
            }
            actuals.push(text);
        }
        (callee, actuals.join(", "))
    }

    /// The C name of a module-qualified designator, resolving an import *alias*
    /// to the module it stands for (`IMPORT O := Out; O.WriteLn` is
    /// `Out__WriteLn`).
    fn qualified_cname_for(&self, base: &[String]) -> Option<String> {
        if base.len() != 2 {
            return None;
        }
        self.module_of_alias(&base[0])
            .map(|m| format!("{}__{}", mangle(&m), mangle(&base[1])))
    }

    fn module_of_alias(&self, alias: &str) -> Option<String> {
        self.aliases.get(alias).cloned()
    }

    /// A parameter the callee receives as a pointer: a `VAR` parameter, and a
    /// Record value parameters use a caller-made copy because C lacks Oberon's
    /// read-only value-parameter rule. Array value parameters already copy their
    /// struct by value.
    fn param_is_pointer(&self, pm: &Param) -> bool {
        pm.by_var || self.types.is_record(pm.ty)
    }

    /// Return an assignable designator for builtins that take a `VAR` argument,
    /// such as `NEW`, `INC`, or `PACK`.
    fn designator_of_mut(&mut self, e: &Expr, level: usize) -> String {
        match &e.kind {
            ExprKind::Designator(d) => self.designator(d, level, true),
            _ => self.designator_of(e, level),
        }
    }

    fn designator_of(&mut self, e: &Expr, level: usize) -> String {
        match &e.kind {
            ExprKind::Designator(d) => self.designator(d, level, false),
            _ => self.expr(e, None),
        }
    }

    fn open_array_actual(&mut self, a: &Expr, elem: TypeId, level: usize) -> (String, String) {
        let t = self.sema.type_at(a.span, self.types).unwrap_or(TypeId(0));
        match &a.kind {
            ExprKind::Str(text) => (format!("\"{}\"", escape_c(text)), text.len().to_string()),
            _ => {
                let _ = elem;
                let v = self.designator_of(a, level);
                match self.types.get(t) {
                    // A fixed array passes its element storage, preserving its
                    // extent for the C compiler.
                    Type::Array { len, .. } => (format!("({v}).e"), len.to_string()),
                    // An open array is already an element pointer. Forward its
                    // hidden length without taking the parameter's address.
                    Type::OpenArray { .. } => (v.clone(), format!("{v}__len")),
                    Type::StringLit(n) => (format!("((char *)({v}))"), n.to_string()),
                    _ => (v, "0".into()),
                }
            }
        }
    }

    // Predefined calls

    fn predef_proper(&mut self, name: &str, d: &ast::Designator, level: usize, out: &mut String) {
        let args = match d.sels.last() {
            Some(Selector::Call(a)) => a.clone(),
            _ => Vec::new(),
        };
        match name {
            "INC" | "DEC" => {
                let v = self.designator_of_mut(&args[0], level);
                let n = match args.get(1) {
                    Some(a) => self.expr(a, None),
                    None => "1".into(),
                };
                let op = if name == "INC" { "+=" } else { "-=" };
                let pre = self.take_prelude();
                out.push_str(&pre);
                let _ = writeln!(out, "    {v} {op} {n};");
            }
            "INCL" | "EXCL" => {
                let v = self.designator_of_mut(&args[0], level);
                let x = self.expr(&args[1], None);
                let pre = self.take_prelude();
                out.push_str(&pre);
                if name == "INCL" {
                    let _ = writeln!(out, "    {v} |= rt_set({x}, {x});");
                } else {
                    let _ = writeln!(out, "    {v} &= ~rt_set({x}, {x});");
                }
            }
            "ASSERT" => {
                let b = self.expr(&args[0], Some(self.types.boolean()));
                let pre = self.take_prelude();
                out.push_str(&pre);
                let _ = writeln!(
                    out,
                    "    rt_guard_at({b}, \"assertion failed\", {});",
                    d.span.lo
                );
            }
            "NEW" => {
                let p = self.designator_of_mut(&args[0], level);
                let pre = self.take_prelude();
                out.push_str(&pre);
                let ty = self.sema.type_at(args[0].span, self.types).unwrap_or(TypeId(0));
                let base = self.types.deref(ty).unwrap_or(TypeId(0));
                let bt = self.c_type(base);
                let desc = self.descriptor_name(base);
                // Allocate through the runtime to store the object's descriptor
                // at its head, where guards and type CASE read it. The runtime
                // also returns NIL on failure (section 6.4) and calls the
                // heap-exhaustion hook used by the trap matrix.
                let _ = writeln!(out, "    {p} = rt_new(sizeof({bt}), {desc});");
                let _ = writeln!(out, "    if ({p} != NULL) {{");
                // Records *inside* the new object get their descriptors too.
                self.desc_init(base, &format!("(*{p})"), out, "        ");
                let _ = writeln!(out, "    }}");
            }
            "PACK" => {
                // PACK scales x by 2^n, so n is a value (report 10.2).
                let x = self.designator_of_mut(&args[0], level);
                let n = self.expr(&args[1], None);
                let pre = self.take_prelude();
                out.push_str(&pre);
                let _ = writeln!(out, "    rt_pack(&({x}), {n});");
            }
            "UNPK" => {
                // UNPK writes both the mantissa and the exponent.
                let x = self.designator_of_mut(&args[0], level);
                let n = self.designator_of_mut(&args[1], level);
                let pre = self.take_prelude();
                out.push_str(&pre);
                let _ = writeln!(out, "    rt_unpk(&({x}), &({n}));");
            }
            _ => {}
        }
    }

    fn predef_function(&mut self, name: &str, args: &[Expr], level: usize) -> String {
        let one = |me: &mut Self, i: usize, want: Option<TypeId>| -> String {
            match args.get(i) {
                Some(a) => me.expr(a, want),
                None => "0".into(),
            }
        };
        match name {
            "ABS" => {
                let t = args
                    .first()
                    .and_then(|a| self.sema.type_at(a.span, self.types))
                    .unwrap_or(TypeId(0));
                // Use a helper because inline expansion would evaluate the
                // operand more than once. Calls may return different results.
                // `ABS` also accepts REAL and preserves its operand type (report
                // 10.2), so choose the helper by type.
                let helper = if matches!(self.types.get(t), Type::Basic(Basic::Real)) {
                    "rt_absf"
                } else {
                    "rt_abs"
                };
                format!("{helper}({})", one(self, 0, Some(t)))
            }
            "ODD" => {
                let x = one(self, 0, None);
                format!("((({x}) & 1) != 0)")
            }
            "LSL" => format!("rt_lsl({}, {})", one(self, 0, None), one(self, 1, None)),
            "ASR" => format!("rt_asr({}, {})", one(self, 0, None), one(self, 1, None)),
            "ROR" => format!("rt_ror({}, {})", one(self, 0, None), one(self, 1, None)),
            "FLOOR" => format!("((int32_t)rt_floor({}))", one(self, 0, None)),
            "FLT" => format!("((float)({}))", one(self, 0, None)),
            "ORD" => {
                let t = args
                    .first()
                    .and_then(|a| self.sema.type_at(a.span, self.types))
                    .unwrap_or(TypeId(0));
                // `ORD("0")` is allowed by the report (10.1). A one-character
                // string is a CHAR, which must be read unsigned.
                if let Some(Expr {
                    kind: ExprKind::Str(text),
                    ..
                }) = args.first()
                {
                    return format!("((int32_t)(unsigned char)\"{}\"[0])", escape_c(text));
                }
                let want = if self.types.is_char(t) || self.types.char() == t { Some(t) } else { None };
                let x = one(self, 0, want);
                if self.types.is_char(t) {
                    format!("((int32_t)(unsigned char)({x}))")
                } else {
                    format!("((int32_t)({x}))")
                }
            }
            "CHR" => format!("((char)({}))", one(self, 0, None)),
            "LEN" => {
                let t = args
                    .first()
                    .and_then(|a| self.sema.type_at(a.span, self.types))
                    .unwrap_or(TypeId(0));
                match self.types.get(t) {
                    Type::Array { len, .. } => len.to_string(),
                    Type::OpenArray { .. } => {
                        let v = args
                            .first()
                            .map(|a| self.designator_of(a, level))
                            .unwrap_or_default();
                        format!("{v}__len")
                    }
                    _ => "0".into(),
                }
            }
            _ => "0".into(),
        }
    }

    // SYSTEM builtins (report 11.1)

    /// The `SYSTEM.<entry>` a designator names, if any. The import alias makes
    /// it an entry, so `IMPORT S := SYSTEM` works. No other alias qualifies
    /// because SYSTEM is resolved before interfaces (DESIGN 9.10).
    fn system_entry<'b>(&self, base: &'b [String]) -> Option<&'b str> {
        if base.len() != 2 {
            return None;
        }
        match self.module_of_alias(&base[0]) {
            Some(m) if m == "SYSTEM" => Some(&base[1]),
            _ => None,
        }
    }

    /// The mask for narrowing a word to a smaller type. `CHAR` is one byte
    /// (report 9.2); other basic types accepted by `GET`/`PUT` use the full word
    /// stored by report 11.1's `mem[a]`.
    fn word_mask(&self, t: TypeId) -> Option<&'static str> {
        if self.types.is_char(t) {
            Some("0xFFu")
        } else {
            None
        }
    }

    /// `SYSTEM.GET(a, v)` reads one word through byte address `a`, narrowed to
    /// `v`'s type. A `REAL` read returns its bit pattern; converting an integer
    /// to `float` follows `FLT` semantics.
    fn sys_load(&self, addr: &str, t: TypeId) -> String {
        if matches!(self.types.get(t), Type::Basic(Basic::Real)) {
            return format!("(*(const float *)(uintptr_t)({addr}))");
        }
        match self.word_mask(t) {
            Some(m) => format!(
                "(({})((*(const uint32_t *)(uintptr_t)({addr})) & {m}))",
                self.c_type(t)
            ),
            None => format!(
                "(({})(*(const uint32_t *)(uintptr_t)({addr})))",
                self.c_type(t)
            ),
        }
    }

    /// `SYSTEM.PUT(a, x)` stores the word occupied by `x` through byte address
    /// `a`. `CHAR` uses its low byte, `REAL` its bit pattern, and integers the
    /// full word.
    fn sys_store(&self, addr: &str, t: TypeId, value: &str) -> String {
        if matches!(self.types.get(t), Type::Basic(Basic::Real)) {
            return format!("(*(float *)(uintptr_t)({addr}) = ({value}))");
        }
        match self.word_mask(t) {
            Some(m) => format!(
                "(*(uint32_t *)(uintptr_t)({addr}) = ((uint32_t)({value}) & {m}))"
            ),
            None => format!("(*(uint32_t *)(uintptr_t)({addr}) = (uint32_t)({value}))"),
        }
    }

    /// Whether `e` is a `SYSTEM.ADR` call. Its C value has host address width
    /// (R31), unlike its Oberon type. It has no side effect and needs neither
    /// narrowing to its Oberon `INTEGER` type nor sequencing.
    fn is_address(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Designator(d) => matches!(self.system_entry(&d.base), Some("ADR")),
            _ => false,
        }
    }

    /// Fix an operand in a temporary when any call argument has side effects.
    /// Oberon evaluates operands left to right, but C leaves their order
    /// unspecified, so sequence them as call arguments are sequenced (R23).
    /// `seq` describes the whole call, so temporaries follow source order.
    fn sys_operand(&mut self, e: &Expr, seq: bool, want: Option<TypeId>) -> String {
        let text = self.expr(e, want);
        match (seq, self.sema.type_at(e.span, self.types)) {
            (true, Some(t)) if !self.is_address(e) => self.materialise(t, &text),
            _ => text,
        }
    }

    /// `SYSTEM.GET`, `SYSTEM.PUT`, and `SYSTEM.COPY` are proper procedures
    /// lowered here (DESIGN 9.10). They ignore `self.checks`; `-Ochecks=n` does
    /// not add or remove checks for SYSTEM.
    fn sys_proper(&mut self, entry: &str, d: &ast::Designator, level: usize, out: &mut String) {
        let args = match d.sels.last() {
            Some(Selector::Call(a)) => a.clone(),
            // Unreachable: the checker reports rows 59 and 30 first.
            _ => Vec::new(),
        };
        let seq = args.iter().any(|a| self.has_call(a));
        let text = match entry {
            "GET" | "PUT" => {
                let addr = self.sys_operand(&args[0], seq, Some(self.types.integer()));
                // The typed operand sets the width, based on the report's
                // signatures. `GET(a, v)` names the variable it writes, while
                // `PUT(a, x)` names the value it stores (DESIGN 9.10).
                let t = self.sema.type_at(args[1].span, self.types).unwrap_or(TypeId(0));
                if entry == "GET" {
                    let place = self.designator_of_mut(&args[1], level);
                    format!("{place} = {};", self.sys_load(&addr, t))
                } else {
                    let value = self.sys_operand(&args[1], seq, None);
                    format!("{};", self.sys_store(&addr, t, &value))
                }
            }
            "COPY" => {
                // The report defines `mem` as an array of words, so `n` counts
                // words and the byte count is `n * 4` (DESIGN 9.10). There is no
                // bounds check or overlap guarantee. `memcpy` copies the words.
                let src = self.sys_operand(&args[0], seq, None);
                let dst = self.sys_operand(&args[1], seq, None);
                let n = self.sys_operand(&args[2], seq, None);
                format!(
                    "memcpy((void *)(uintptr_t)({dst}), (const void *)(uintptr_t)({src}), \
                     (size_t)({n}) * 4u);"
                )
            }
            _ => {
                // Filtered by `SYSTEM_PROPER` at the call site.
                self.unresolved += 1;
                String::new()
            }
        };
        let pre = self.take_prelude();
        out.push_str(&pre);
        let _ = writeln!(out, "    {text}");
    }

    /// `SYSTEM.ADR`, `SYSTEM.SIZE`, and `SYSTEM.BIT` are function procedures
    /// lowered here (DESIGN 9.10). The `ob_sys_` namespace remains reserved for
    /// the runtime.
    fn sys_function(&mut self, entry: &str, args: &[Expr], level: usize) -> String {
        match entry {
            // Return the variable's address in SYSTEM's byte-address model.
            // Use `intptr_t`, not the report's `INTEGER`: `int32_t` cannot hold
            // a stack or `mmap` address on a 64-bit host. Converting a truncated
            // value back through `(uint32_t *)(uintptr_t)a` would dereference the
            // wrong address. The expression has host address width, while a
            // stored `INTEGER` follows the report's 32-bit model (R31).
            "ADR" => {
                let place = self.designator_of_mut(&args[0], level);
                format!("((intptr_t)(uintptr_t)&({place}))")
            }
            // `SIZE(T)` is the byte size in the layout, used by `NEW` and record
            // descriptors. The C compiler folds `sizeof` (DESIGN 9.10).
            "SIZE" => match self.sema.type_at(args[0].span, self.types) {
                Some(t) => format!("((int32_t)sizeof({}))", self.c_type(t)),
                None => {
                    // Unreachable: the checker types the argument (row 55).
                    self.unresolved += 1;
                    "0".into()
                }
            },
            // Bit `n` of the word at `a`. Mask the count to 0..31, as for runtime
            // shifts (R22). The report leaves other counts undefined; an
            // unmasked shift would be undefined C.
            "BIT" => {
                let seq = args.iter().any(|a| self.has_call(a));
                let a = self.sys_operand(&args[0], seq, Some(self.types.integer()));
                let n = self.sys_operand(&args[1], seq, Some(self.types.integer()));
                format!(
                    "(((*(const uint32_t *)(uintptr_t)({a}) >> ((uint32_t)({n}) & 31u)) & 1u) != 0)"
                )
            }
            _ => {
                // Filtered by `SYSTEM_FUNCTIONS` at the call site.
                self.unresolved += 1;
                String::new()
            }
        }
    }

    // Constants

    /// Render a constant as C from the checker's folded value (DESIGN 9.2).
    /// This is the only conversion to C, so it uses the folder's result.
    fn const_cvalue(&self, v: &sema::const_eval::Const) -> String {
        use sema::const_eval::Const as K;
        match v {
            K::Int(n) => format!("((int32_t){}u)", *n as u32),
            K::Real(r) => format!("((float){r:?}f)"),
            K::Bool(b) => if *b { "1".into() } else { "0".into() },
            K::Char(c) => format!("((char){}u)", c),
            K::Set(bits) => format!("((uint32_t){}u)", bits),
            K::Str(s) => format!("((const char *)\"{}\"", escape_c(s)) + ")",
        }
    }

    /// Render a constant in Oberon notation for an interface file; the reader
    /// parses it with the grammar (DESIGN 7).
    fn const_render(v: &sema::const_eval::Const) -> String {
        use sema::const_eval::Const as K;
        match v {
            K::Int(n) => n.to_string(),
            K::Real(r) => format!("{r:?}"),
            K::Bool(b) => if *b { "TRUE".into() } else { "FALSE".into() },
            K::Char(c) => format!("{c:02X}X"),
            K::Str(s) => format!("\"{s}\""),
            K::Set(bits) => {
                // Maximal runs, so a set stays short and the reader (which
                // accepts a list of ranges) reads it back.
                let mut parts = Vec::new();
                let mut i = 0u32;
                while i < 32 {
                    if bits & (1 << i) == 0 {
                        i += 1;
                        continue;
                    }
                    let lo = i;
                    while i + 1 < 32 && bits & (1 << (i + 1)) != 0 {
                        i += 1;
                    }
                    let hi = i;
                    parts.push(if lo == hi {
                        lo.to_string()
                    } else {
                        format!("{lo}..{hi}")
                    });
                    i += 1;
                }
                format!("{{{}}}", parts.join(", "))
            }
        }
    }

    fn const_value(&self, e: &Expr) -> String {
        match &e.kind {
            ExprKind::Int(v) => format!("((int32_t){}u)", v),
            ExprKind::Real(v) => format!("((float){v:?}f)"),
            ExprKind::Char(c) => format!("((char){}u)", c),
            ExprKind::Bool(b) => if *b { "1".into() } else { "0".into() },
            ExprKind::Str(text) => format!("((const char *)\"{}\")", escape_c(text)),
            ExprKind::Set(_) => match sema::const_eval::eval(e) {
                Ok(sema::const_eval::Const::Set(bits)) => format!("((uint32_t){}u)", bits),
                _ => "0u".into(),
            },
            ExprKind::Unary(UnOp::Neg, x) => format!("(-({}))", self.const_value(x)),
            ExprKind::Binary(op, l, r) => {
                let a = self.const_value(l);
                let b = self.const_value(r);
                match op {
                    BinOp::Add => format!("(({a}) + ({b}))"),
                    BinOp::Sub => format!("(({a}) - ({b}))"),
                    BinOp::Mul => format!("(({a}) * ({b}))"),
                    BinOp::Div => format!("(({a}) / ({b}))"),
                    _ => "0".into(),
                }
            }
            _ => "0".into(),
        }
    }

    // Interface text

    /// Render a type in the form used by interface files (DESIGN 7). The
    /// `RenderedTypeParser` reads the output with the module grammar.
    pub fn render_type(&self, t: TypeId) -> String {
        // Use a declared type's name to preserve record identity. The reader
        // builds the whole interface at once, and inlining a recursive record
        // would not end.
        if let Some(n) = self.oberon_names.get(&t) {
            return n.clone();
        }
        if let Some(q) = self.sema.qualified_name(t) {
            return q;
        }
        self.render_type_body(t)
    }

    /// The body of a type, for the one line that declares it.
    fn render_type_body(&self, t: TypeId) -> String {
        match self.types.get(t) {
            Type::Basic(Basic::Integer) => "INTEGER".into(),
            Type::Basic(Basic::Byte) => "BYTE".into(),
            Type::Basic(Basic::Real) => "REAL".into(),
            Type::Basic(Basic::Char) => "CHAR".into(),
            Type::Basic(Basic::Boolean) => "BOOLEAN".into(),
            Type::Basic(Basic::Set) => "SET".into(),
            Type::Array { len, elem } => format!("ARRAY {len} OF {}", self.render_type(*elem)),
            Type::OpenArray { elem } => format!("ARRAY OF {}", self.render_type(*elem)),
            Type::Pointer { base } => format!("POINTER TO {}", self.render_type(*base)),
            Type::Record { base, fields, .. } => {
                // Write out anonymous records so clients can lay them out. Their
                // fields use semicolons and include the public mark (report 6.3).
                let mut text = match base {
                    Some(b) => format!("RECORD ({})", self.render_type(*b)),
                    None => "RECORD".to_string(),
                };
                for f in fields {
                    // Only the field's own export mark lets a client name it
                    // (report 6.3).
                    let mark = if f.export { "*" } else { "" };
                    let _ = write!(text, " {}{mark}: {};", mangle(&f.name), self.render_type(f.ty));
                }
                // Drop the separator after the last field. `FieldListSequence`
                // is semicolon-separated (report 6.3).
                if !fields.is_empty() {
                    text.pop();
                }
                text.push_str(" END");
                text
            }
            Type::Proc { params, result } => {
                let mut parts = Vec::new();
                for p in params {
                    let ty = self.render_type(p.ty);
                    parts.push(if p.by_var { format!("VAR {ty}") } else { ty });
                }
                let mut text = format!("PROCEDURE ({})", parts.join("; "));
                if let Some(r) = result {
                    let _ = write!(text, ": {}", self.render_type(*r));
                }
                text
            }
            Type::Nil => "NIL".into(),
            Type::StringLit(n) => format!("ARRAY {} OF CHAR", n + 1),
            Type::Error => "INTEGER".into(),
        }
    }
}

/// A checked module's interface lists what clients may refer to (DESIGN 7).
/// It includes only exported objects; exported variables are read-only to
/// clients (report 11).
pub fn interface_of(module: &Module, types: &Types, sema: &Sema<'_>) -> sema::sym::Interface {
    let mut e = Emitter::new(module.name.clone(), types, sema);
    let mut wanted = Vec::new();
    e.collect_types(&module.decls, &mut wanted);
    let mut iface = sema::sym::Interface {
        module: module.name.clone(),
        imports: module.imports.iter().map(|i| i.module.clone()).collect(),
        items: Vec::new(),
    };
    for c in &module.decls.consts {
        if c.export {
            let value = match sema.const_of(c.span) {
                Some((v, _)) => Emitter::const_render(v),
                None => e.const_value(&c.value),
            };
            iface.items.push(sema::sym::Item::Const {
                name: c.name.clone(),
                value,
            });
        }
    }
    for t in &module.decls.types {
        if t.export {
            let ty = e.decl_type(&t.ty);
            iface.items.push(sema::sym::Item::Type {
                name: t.name.clone(),
                def: e.render_type_body(ty),
            });
        }
    }
    for v in &module.decls.vars {
        let ty = e.decl_type(&v.ty);
        for (name, export) in &v.names {
            if *export {
                iface.items.push(sema::sym::Item::Var {
                    name: name.clone(),
                    ty: e.render_type(ty),
                });
            }
        }
    }
    for p in &module.decls.procs {
        if !p.export {
            continue;
        }
        let (flat, result) = match sema.signatures.get(&sema::sema::span_key(p.span)) {
            Some(sig) => (sig.params.clone(), sig.result),
            None => (Vec::new(), None),
        };
        iface.items.push(sema::sym::Item::Proc {
            name: p.name.clone(),
            params: flat
                .iter()
                .map(|(n, pm)| {
                    let ty = e.render_type(pm.ty);
                    if pm.by_var {
                        format!("VAR {n}: {ty}")
                    } else {
                        format!("{n}: {ty}")
                    }
                })
                .collect(),
            result: result.map(|r| e.render_type(r)),
        });
    }
    iface
}

/// The span where a `TypeExpr` was parsed. The semantic pass uses it as the
/// key for the expression's type (DESIGN 6).
///
/// Shared with the Cranelift back end, which resolves declaration types the same
/// way. Keeping one function means both back ends handle every `TypeExpr`.
pub fn span_of_type(t: &TypeExpr) -> Span {
    match t {
        TypeExpr::Named(_, s)
        | TypeExpr::Array { span: s, .. }
        | TypeExpr::Record { span: s, .. }
        | TypeExpr::Pointer(_, s)
        | TypeExpr::Proc(_, s) => *s,
    }
}

/// An Oberon identifier cannot contain `__`, so user code cannot use generated
/// names such as module prefixes, frames, or temporaries.
///
/// Shared with the Cranelift back end, which emits the same `<Module>__<Name>`
/// symbols. Programs mixing both back ends link those symbols together
/// (DESIGN 16.2), so both need the same mangling.
pub fn mangle(name: &str) -> String {
    let name = name.replace('.', "__");
    // Oberon identifiers are case-sensitive, so names such as `int`, `while`,
    // and `char` are legal in Oberon but reserved in C. Prefix keywords with
    // `o_`. Generated names already use `Module__Name` (which Oberon identifiers
    // cannot contain) or reserved runtime prefixes such as `TD_`, `rt_`, and `ob_`.
    if C_KEYWORDS.contains(&name.as_str()) {
        format!("o_{name}")
    } else {
        name
    }
}

const C_KEYWORDS: &[&str] = &[
    "auto", "break", "case", "char", "const", "continue", "default", "do", "double", "else",
    "enum", "extern", "float", "for", "goto", "if", "inline", "int", "long", "register",
    "restrict", "return", "short", "signed", "sizeof", "static", "struct", "switch", "typedef",
    "union", "unsigned", "void", "volatile", "while", "_Bool", "_Complex", "_Imaginary",
];

/// A C string literal body: printable ASCII verbatim, everything else as a hex
/// escape (Oberon strings have no escape sequences of their own).
fn escape_c(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x20..=0x7e => out.push(b as char),
            _ => {
                let _ = write!(out, "\\x{b:02x}\"\"");
            }
        }
    }
    out
}
