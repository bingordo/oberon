//! The C back end (DESIGN §9): walk the checked module and emit C11.
//!
//! Every emission rule here is the one verified in `oberon/csmoke` (runtime
//! semantics: DIV/MOD, sets, shifts, descriptors, links, parameter modes) and
//! `oberon/golden` (statement lowering, module and command model). The emitter
//! performs no optimisation: the C compiler does that.
//!
//! One Oberon module becomes `Foo.c` + `Foo.h` + `Foo.sym`; the shared runtime
//! is `oberon.h` + `oberon_runtime.c` (the two `.inc` files next to this one),
//! so the registry and the descriptors have one identity across modules.
//!
//! Three representation decisions shape everything below:
//!
//! * A fixed-size `ARRAY n OF T` becomes a **named struct with one field**,
//!   `struct { T e[n]; }`. Assignment is then C struct assignment (a copy, no
//!   `memcpy` bookkeeping), passing is passing a pointer, and indexing is
//!   `x.e[i]`. The name is derived from the type's id, so two variables of the
//!   same Oberon array type have the same C type and assign to each other. An
//!   Oberon identifier cannot contain `__`, which makes generated names
//!   collision-free.
//! * An open-array parameter becomes two C parameters, `T *a, int32_t a__len`
//!   (report §10.1); `LEN(a)` is that hidden length.
//! * A procedure that declares nested procedures owns a frame struct and keeps
//!   its storage in it; a nested procedure takes a hidden `void *sl` and reaches
//!   enclosing storage by walking links — the scheme `oberon/csmoke` verified,
//!   including that a link-taking procedure is not command-shaped.

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::ast::{self, BinOp, CaseLabel, Expr, ExprKind, Module, QIdent, Selector, Span, Stmt, StmtKind, TypeExpr, UnOp};
use crate::const_eval;
use crate::sema::Sema;
use crate::types::{Basic, Field, Param, Type, TypeId, Types};

/// `oberon.h`, emitted alongside the generated modules.
pub const RUNTIME_HEADER: &str = include_str!("oberon_h.inc");
/// `oberon_runtime.c`, emitted once per program.
pub const RUNTIME_SOURCE: &str = include_str!("oberon_runtime.inc");

const PREDEFINED_PROPER: &[&str] = &["INC", "DEC", "INCL", "EXCL", "NEW", "ASSERT", "PACK", "UNPK"];
const PREDEFINED_FUNCTIONS: &[&str] =
    &["ABS", "ODD", "LEN", "LSL", "ASR", "ROR", "FLOOR", "FLT", "ORD", "CHR"];

/// What an identifier means at emission time.
#[derive(Clone)]
struct Sym {
    /// How the name is written at this point in the output.
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

pub struct Emitter<'a> {
    types: &'a Types,
    sema: &'a Sema<'a>,
    module: String,
    /// C struct names for record and array types.
    type_names: HashMap<TypeId, String>,
    /// The Oberon name of every type this module declares, so an interface can
    /// refer to it the way its reader spells it.
    oberon_names: HashMap<TypeId, String>,
    /// How many type references the emitter could not resolve. A checked module
    /// has none; if one appears, the emitted C carries a `#error` rather than
    /// silently doing something else.
    unresolved: usize,
    /// Import alias -> the module it stands for.
    aliases: HashMap<String, String>,
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
    /// Statements hoisted before the construct being emitted (the temporaries
    /// for structured value parameters). Flushed exactly where the construct is
    /// evaluated once — never out of a loop.
    prelude: String,
    anon: usize,
    /// Non-zero while rendering the right operand of `&&` or `||`, whose value
    /// must not be computed unless the operand is reached.
    in_conditional: usize,
}

pub fn emit_module(module: &Module, types: &Types, sema: &Sema<'_>) -> (String, String) {
    let mut e = Emitter::new(module.name.clone(), types, sema);
    e.run(module)
}

impl<'a> Emitter<'a> {
    fn new(module: String, types: &'a Types, sema: &'a Sema<'a>) -> Emitter<'a> {
        Emitter {
            types,
            sema,
            module,
            type_names: HashMap::new(),
            oberon_names: HashMap::new(),
            unresolved: 0,
            aliases: HashMap::new(),
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
        }
    }

    // ------------------------------------------------------------- top level

    fn run(&mut self, module: &Module) -> (String, String) {
        for imp in &module.imports {
            self.aliases.insert(imp.alias.clone(), imp.module.clone());
        }
        // 1. Every composite type the module mentions, named, then emitted in
        //    dependency order (element before array, base before derived).
        let mut wanted = Vec::new();
        self.collect_types(&module.decls, &mut wanted);
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

        // 4. Procedures in declaration order: Oberon always declares before use,
        //    so C needs no forward declarations beyond the header's prototypes.
        let mut procs = String::new();
        for p in &module.decls.procs {
            self.procedure(p, &mut procs);
        }

        // 5. The module body and the command table (report §11).
        let mut tail = String::new();
        self.frames.push(Frame { frame: String::new(), level: 1, path: "init".into() });
        self.push_scope();
        let _ = writeln!(tail, "void {}__init(void) {{", self.module);
        // Oberon does not require a module to read its own variables, and a
        // `static` C variable the file never mentions is a `-Werror` failure
        // (R20). Marking them costs a read of a zeroed static, exactly as the
        // parameter marks do (PLAN.md W6.2).
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
        // The modules this one imports, so their types and prototypes are
        // visible to whoever includes this header (DESIGN §9.8).
        for imp in &module.imports {
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
            // Only a parameterless procedure can be a command, and only then is
            // the `void (*)(void)` entry sound (report §11; risk R12).
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

    // ------------------------------------------------------------ new types

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
                // The checker recorded an FPSection's type under the procedure's
                // span (it flattens the section there), which is the key used
                // here too.
                let key = span_of_type(&TypeExpr::Named(sec.ty.base.clone(), p.span));
                if let Some(t) = self.sema.type_at(key, self.types) {
                    self.name_type(t, "");
                }
            }
            self.collect_types(&p.decls, _wanted);
        }
    }

    fn name_type(&mut self, t: TypeId, declared: &str) {
        // A type that came from an interface file belongs to that module and is
        // defined by its header; naming it here again would create a second,
        // incompatible C type (DESIGN §9.8).
        if self.sema.qualified_name(t).is_some() {
            return;
        }
        // Naming a composite *before* visiting its dependencies is what makes
        // recursive types terminate: `P = POINTER TO R; R = RECORD next: P END`
        // comes back to R, which is already named and therefore already done.
        match self.types.get(t).clone() {
            Type::Array { elem, .. } => {
                let name = if declared.is_empty() {
                    format!("{}__Arr_{}", self.module, t.0)
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
                    self.anon += 1;
                    format!("{}__Anon_{}", self.module, self.anon)
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
                let bname = self.type_c_name(base).unwrap_or_default();
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
                // A by-value field must be complete before this record, and so
                // must the base it embeds. Pointer fields impose no order.
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
                // The descriptor lives in the base-most record of the chain
                // (oberon/csmoke's verified layout): a derived record starts
                // with its base, so `*(ObDesc **)p` is the same word for a
                // Base* and a Derived* - which is what `IS`, a guard and a
                // type CASE rely on.
                match base {
                    Some(b) => {
                        // The base may belong to an imported module, in which
                        // case its name comes from that module's namespace.
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
                let _ = writeln!(
                    self.descriptors,
                    "const ObDesc TD_{name} = {{ {base_ref}, {}u, \"{name}\" }};",
                    t.0
                );
                let _ = writeln!(self.header, "extern const ObDesc TD_{name};");
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------ C binding

    fn binding(&self, t: TypeId, name: &str) -> String {
        // A procedure type is a *declarator*, not a type name, so the variable
        // has to go inside it: `int32_t (*f)(int32_t)`.
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

    /// The type of a declaration's `TypeExpr`: the semantic pass recorded every
    /// type expression by span (DESIGN §6).
    fn decl_type(&self, t: &TypeExpr) -> TypeId {
        self.sema.type_at(span_of_type(t), self.types).unwrap_or(TypeId(0))
    }

    // ----------------------------------------------------------------- scope

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

    // ----------------------------------------------------------- procedures

    fn procedure(&mut self, p: &ast::Proc, out: &mut String) {
        let level = self.frames.len() + 1;
        let path = match self.frames.last() {
            Some(f) if level > 1 => format!("{}__{}", f.path, p.name),
            _ => p.name.clone(),
        };
        let cname = format!("{}__{}", self.module, mangle(&path));
        let has_nested = !p.decls.procs.is_empty();
        let frame = format!("F_{}", mangle(&path));

        // The signature as the checker flattened it, and its procedure type.
        let (pty, flat, result) = match self.sema.signatures.get(&crate::sema::span_key(p.span)) {
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
            // report §6.5 / DESIGN §9.7: a nested procedure takes a static link.
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
        for s in &p.body {
            self.stmt(s, out, level);
        }
        if let (Some(want), Some(e)) = (result, &p.ret) {
            let v = self.expr(e, Some(want));
            let pre = self.take_prelude();
            out.push_str(&pre);
            let _ = writeln!(out, "    return {v};");
        }
        let _ = writeln!(out, "}}");
        out.push_str(&nested);

        self.frames.pop();
        self.pop_scope();
    }

    /// The type a name denotes at this point: a type declared in the current
    /// procedure or module, or one imported as `M.T`.
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

    // ----------------------------------------------------------- statements

    fn take_prelude(&mut self) -> String {
        std::mem::take(&mut self.prelude)
    }

    /// Fixes a value in the prelude and hands back the name it was fixed under.
    ///
    /// Oberon's reference compilers evaluate operands left to right, while C
    /// leaves the order of the operands of `+`, `*`, `/` — and of a call's
    /// arguments — unspecified, so `m0 + R(3)` where `R` writes `m0` may come out
    /// either way. Fixing both operands in source order makes the emitted C say
    /// what the language says (DESIGN R23; the generator found it, PLAN.md W6.2).
    fn materialise(&mut self, t: TypeId, value: &str) -> String {
        self.anon += 1;
        let tmp = format!("tmp__{}", self.anon);
        let decl = self.binding(t, &tmp);
        if self.in_conditional > 0 {
            // Inside a `&&`/`||` operand the value must not be computed unless
            // that operand is reached, so only the declaration is hoisted (it
            // evaluates nothing) and the assignment stays where the value is —
            // a comma expression, which C sequences.
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
        match &s.kind {
            StmtKind::Assign { target, value } => {
                let dst = self.designator(target, level, true);
                let dst_ty0 = self.sema.type_at(target.span, self.types);
                let src_ty0 = self.sema.type_at(value.span, self.types);
                let pre = self.take_prelude();
                out.push_str(&pre);
                let dst_ty = self.sema.type_at(target.span, self.types);
                // A string literal into a CHAR array is a padded copy, which is
                // a call and not an assignment (report §9.1.2).
                if let (Some(dt), ExprKind::Str(text)) = (dst_ty, &value.kind) {
                    if let Type::Array { len, elem } = self.types.get(dt) {
                        if self.types.is_char(*elem) {
                            let _ = writeln!(out, "    rt_strcopy({dst}.e, {len}, \"{}\");", escape_c(text));
                            return;
                        }
                    }
                }
                // A record assigned from another record *type* (an extension
                // up-cast) keeps the destination's static descriptor: a plain C
                // struct assignment would copy the source's, making a base
                // variable advertise a derived dynamic type (DESIGN §9.3).
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
                // leaves no terminator (risk R14).
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
                // report §9.6: repeat until no guard is true. A guard's
                // temporary goes inside the loop, so it is remade per iteration.
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
                // `do … while (!c)` would move a temporary out of the loop, so
                // the explicit form is always emitted.
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
            // DESIGN 9.3: switch on the descriptor id; inside an arm the case
            // variable has the label's type (report §9.5), which is modelled by
            // rebinding the name in an arm-local scope.
            let desc = self.dyn_of(&subj, sub_ty);
            let _ = writeln!(out, "    switch (({desc})->id) {{");
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
            if !default_taken {
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
                    // A second label-less arm in one CASE is unreachable code in
                    // Oberon too; C tolerates only one `default`.
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
        let _ = writeln!(out, "    }}");
    }

    /// The descriptor of the record a designator denotes: for a POINTER, the
    /// first word of the payload; for a record, the first word of its storage.
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

    /// The right-hand side of an assignment, adjusted where Oberon's and C's
    /// type systems differ (DESIGN §9.6): a derived record into a base-typed
    /// destination needs the base prefix, and two distinct pointer types need a
    /// cast.
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

    // ---------------------------------------------------------- expressions

    fn expr(&mut self, e: &Expr, want: Option<TypeId>) -> String {
        // A constant expression is emitted as its value. The C compiler would
        // fold it anyway; the point is that it also *warns* about the overflow it
        // folds through (`-Woverflow`, and a tautological comparison), and the
        // documented build line treats warnings as errors (R20, PLAN.md W6.2).
        // The folder is the same one DESIGN §8.3 cross-checks against the
        // runtime, so this cannot disagree with the emitted helpers.
        // The resolver is the checker's use-site table: a designator that names a
        // constant is folded to its value, which is what lets `MAX * (1 + c0)`
        // become one literal instead of a C expression that overflows.
        // Two limits on what may be folded, both of them correctness rather than
        // taste: only *structurally* constant trees (`foldable`), because the
        // folder recognises a builtin such as `ABS` by its spelling and a nested
        // procedure or a parameter may legally shadow that name — and a call may
        // have side effects, so it has to reach the emitted C; and only values
        // that are INTEGER or BOOLEAN, emitted in their own C type, because a
        // CHAR taken through `Const::int()` would come out as an `int32_t` and
        // the narrowing assignment is a `-Werror` failure where `char` is signed.
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
                // A one-character string where a CHAR is expected: report §10.1
                // writes `ch <= "9"` and `ORD("0")`.
                if let Some(w) = want {
                    if self.types.is_char(w) {
                        return format!("\"{}\"[0]", escape_c(text));
                    }
                }
                format!("\"{}\"", escape_c(text))
            }
            ExprKind::Designator(d) => {
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
                format!("rt_is({}, {target})", self.dyn_of(&v, ty))
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

    /// The descriptor symbol of a type: a pointer type has none of its own, so
    /// it names its base record's (DESIGN §9.3).
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
        // `x = x` is true and `x < x` is false for every scalar but REAL (where
        // NaN makes it false), yet C compilers warn about the comparison itself
        // and the documented build line treats warnings as errors (R20). Only
        // plainly side-effect-free operands are folded: a call may differ per
        // call, and in Oberon it may have side effects.
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
        // `&&` and `||` are sequenced by C already — and sequencing them by
        // hand would evaluate the right operand eagerly, which is the one thing
        // short-circuiting exists to prevent.
        let sequenced = !matches!(op, BinOp::And | BinOp::Or)
            && (self.has_call(l) || self.has_call(r));
        let array_of_operands = {
            let char_cmp = self.types.is_char_like(lt) && self.types.is_char_like(rt);
            !char_cmp
                && ((self.types.is_char_array(lt) && !self.types.is_char_like(lt))
                    || (self.types.is_char_array(rt) && !self.types.is_char_like(rt)))
        };
        let mut a = self.expr(l, want);
        if sequenced && !array_of_operands && !self.is_aggregate(lt) {
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
        if sequenced && !array_of_operands && !self.is_aggregate(rt) {
            b = self.materialise(want.unwrap_or(rt), &b);
        }
        // Two single characters compare as ordinals: a C `char` may be signed,
        // in which case `255X > 1X` would come out backwards (report §8.2.4).
        // A *string* of more than one character is a character array, and goes
        // to the bounded comparison below instead.
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
            // Bounded comparison: an ARRAY n OF CHAR need not be terminated
            // (DESIGN §9.5, risk R14).
            let (na, nb) = (self.char_len(lt), self.char_len(rt));
            if sequenced {
                a = self.materialise_addr("const char", &format!("(const char *)&({a})"));
                b = self.materialise_addr("const char", &format!("(const char *)&({b})"));
            }
            // With the addresses already fixed, the temporaries *are* the
            // arguments; without them the addresses are taken in place.
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
        // report §8.2.3: with a SET operand, `+ - * /` are union, difference,
        // intersection and symmetric difference, and `-` on its own is the
        // complement (handled with the unary operators).
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

    /// Whether evaluating this expression can call something — an Oberon call is
    /// the only side effect an expression can have. Exhaustive, so a new
    /// expression form has to be classified rather than defaulting to "harmless".
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

    /// Whether two operands are the same literal or the same selector-free
    /// designator. `Expr`'s derived `PartialEq` includes source *spans*, so two
    /// occurrences of `x` in `x = x` never compare equal as parsed — which is why
    /// this compares the structure that matters and nothing else.
    /// Two trees that are the same expression and cannot differ between two
    /// evaluations, so `x > x` folds to a constant. Excluded: a call (it may
    /// differ per call and may have side effects) and a selector (a dereference,
    /// an index and a type guard can *trap*, and folding would drop the trap).
    fn same_plain(&self, l: &Expr, r: &Expr) -> bool {
        !self.has_call(l) && self.trap_free(l) && self.same_tree(l, r)
    }

    /// Structural equality that ignores spans: `l.kind == r.kind` would compare
    /// the spans the parser recorded, which differ between two occurrences of
    /// the same designator.
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
            // report §8.2.2: `/` on integers is the quotient the folder computes
            // as DIV (the report defines only DIV/MOD precisely), so it uses the
            // same helper. REAL division and SET symmetric difference are not
            // affected.
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

    /// How a symbol is written where it is used: a plain name, a frame field
    /// reached through the static links, and for a `VAR` parameter a deref of
    /// whichever of those holds the pointer.
    /// The hidden length that accompanies an open-array formal, named the way
    /// `access` names the array itself (through the static links when the
    /// parameter belongs to an enclosing procedure).
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
            // `sl` points at the *parent's* frame, so a symbol one level up
            // needs no hop, one two levels up needs one, and so on.
            let mut link = String::from("sl");
            for _ in 0..(level - s.level - 1) {
                link = format!("(*(void **)({link}))");
            }
            let frame = self.frame_at(s.level);
            format!("(({frame} *){link})->{}", s.cname.trim_start_matches("f."))
        };
        // A `VAR` parameter is a pointer to the actual, so the access dereferences
        // it — except an *open array*, whose parameter is already the pointer to
        // the first element (§9.2), in both modes. Dereferencing that one produced
        // `(*v)[i]`, which does not compile: found by the trap matrix's open-array
        // case (PLAN.md W7.4).
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

    /// Where a field lives: the derived record embeds its base as its first
    /// member (DESIGN §9.3), so an inherited field is `.base.base.field`.
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
    /// its own (DESIGN §9.3). The descriptor word belongs to the outermost
    /// record, so only this function sets it; the base's fields are visited for
    /// their descriptors, never its own descriptor word.
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
        // An open-array formal's length is a hidden companion parameter (§9.2),
        // and the *index* check needs it — only the base of a designator can be an
        // open array, so it is captured once here.
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
                    if is_pointer && !volatile {
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
                            // An open-array parameter is a pointer (report §10.1)
                            // and its length is that companion parameter, so the
                            // bounds check has something to check against. Indexing
                            // an open array used to be emitted with no check at all
                            // — the trap matrix found it (PLAN.md W7.4).
                            match open_len.take() {
                                Some(len) => {
                                    let _ = write!(text, "[rt_index({i}, {len}, {})]", d.span.lo);
                                }
                                None => {
                                    let _ = write!(text, "[{i}]");
                                }
                            }
                        } else {
                            match self.types.array_len(ty) {
                                Some(n) => {
                                    let _ = write!(text, ".e[rt_index({i}, {n}, {})]", d.span.lo);
                                }
                                None => {
                                    let _ = write!(text, ".e[{i}]");
                                }
                            }
                        }
                        // One level per index expression (report §8.1).
                        ty = self.types.elem(ty).unwrap_or(TypeId(0));
                        // An element may be a pointer, and a further dimension
                        // may be an open array.
                        is_pointer = self.types.is_pointer(ty);
                        open = matches!(self.types.get(ty), Type::OpenArray { .. });
                    }
                }
                Selector::Deref => {
                    // Same rule for `p^`: the check is inside the dereference.
                    if is_pointer && !volatile {
                        text = format!("rt_nil({text}, {})", d.span.lo);
                    }
                    text = format!("(*({text}))");
                    ty = self.types.deref(ty).unwrap_or(TypeId(0));
                    is_pointer = self.types.is_pointer(ty);
                }
                Selector::Guard(q) => {
                    // report §8.1: `v(T0)` asserts that v is of type T0 and
                    // *aborts* if it is not. The test is emitted as part of the
                    // expression (a comma expression, not a hoisted statement),
                    // so it keeps its place inside a short-circuit operand, and
                    // a NIL pointer is rejected before its descriptor is read.
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
                            // An assignment's target has to be an *lvalue*, and a
                            // comma expression is not one, so the check becomes a
                            // statement before whatever writes through it — which
                            // every lvalue caller flushes. The cast stays unless
                            // the guard is the whole designator, because a field's
                            // offset belongs to the narrowed record (DESIGN R29).
                            let _ = writeln!(self.prelude, "    {test};");
                            if last {
                                text.clone()
                            } else {
                                view
                            }
                        } else {
                            format!("(({test}, {view}))")
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
                            let _ = writeln!(self.prelude, "    {test};");
                            if last {
                                sub
                            } else {
                                view
                            }
                        } else {
                            format!("(({test}, {view}))")
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

    // ---------------------------------------------------------------- calls

    /// A call used as a statement.
    fn call(&mut self, d: &ast::Designator, level: usize, out: &mut String) {
        let name = d.base[0].clone();
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
        if self.lookup(&name).is_none() && PREDEFINED_FUNCTIONS.contains(&name.as_str()) {
            return self.predef_function(&name, &args, level);
        }
        let mut sink = String::new();
        let (callee, actuals) = self.call_parts(d, level, &mut sink);
        self.prelude.push_str(&sink);
        format!("{callee}({actuals})")
    }

    /// The callee and its argument list, with the two adjustments the report's
    /// parameter modes require: a static link for a nested procedure, and
    /// (pointer, length) for an open array (DESIGN §9.7).
    fn call_parts(&mut self, d: &ast::Designator, level: usize, out: &mut String) -> (String, String) {
        let name = d.base[0].clone();
        let args = match d.sels.last() {
            Some(Selector::Call(a)) => a.clone(),
            _ => Vec::new(),
        };
        // The callee is the designator *without* its argument list: a plain
        // name, a module-qualified name, or a procedure-valued field or
        // element (`r.f(1)`, `a[i](1)`), which the report allows (report §10.1).
        // Either a bare name (`Greet`, a parameterless call) or a name with
        // just its argument list; anything else selects a procedure *value*.
        let plain = d.sels.is_empty() || matches!(d.sels.as_slice(), [Selector::Call(_)]);
        let (callee, callee_ty, level_of_callee, is_declared_proc) = if let Some(c) = self
            .qualified_cname_for(&d.base)
        {
            let ty = self
                .sema
                .call_types
                .get(&crate::sema::span_key(d.span))
                .copied()
                .unwrap_or(TypeId(0));
            (c, ty, 1usize, false)
        } else if plain {
            match self.lookup(&name) {
                Some(sym) => (sym.cname, sym.ty, sym.level, sym.decl_proc),
                None => (mangle(&name), TypeId(0), level, false),
            }
        } else {
            // A selected procedure value: emit it as a designator.
            let head = ast::Designator {
                base: d.base.clone(),
                sels: d.sels[..d.sels.len() - 1].to_vec(),
                span: d.span,
            };
            let ty = self.sema.type_at(d.span, self.types).unwrap_or(TypeId(0));
            (self.designator(&head, level, false), ty, level, false)
        };
        let callee_ty = match self.sema.call_types.get(&crate::sema::span_key(d.span)) {
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
        // C leaves the order of a call's *arguments* unspecified too, and a
        // callee can write a variable another argument reads (the generator
        // found it: `F0(ABS(F0(100, acc)), LSL(ASR(m0, 0), 1))`). A value
        // actual is fixed in a temporary, in source order, when any actual can
        // have a side effect; an address actual has to stay an address.
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
                // report §10.1: a record formal may be a base type of the
                // actual's type, which in C means an explicit up-cast to the
                // embedded base (DESIGN §9.3).
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
                // A record value parameter is passed as a pointer to a copy the
                // caller makes (report §10.1, DESIGN §9.7), and the copy keeps
                // the *formal's* descriptor, not the actual's.
                self.anon += 1;
                let tmp = format!("tmp__{}", self.anon);
                let _ = writeln!(out, "    {};", self.binding(formal.ty, &tmp));
                let src = self.designator_of(a, level);
                let desc = self.descriptor_name(formal.ty);
                let _ = writeln!(
                    out,
                    "    rt_rec_copy(&{tmp}, &({src}), (int32_t)sizeof({tmp}), {desc});"
                );
                actuals.push(format!("&{tmp}"));
                continue;
            }
            let got = self.sema.type_at(a.span, self.types);
            // `want` is the formal's type where the two differ (a one-character
            // string into a CHAR, report §9.1.2), the actual's otherwise.
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
    /// *record* value parameter (the caller hands over a copy it made, so the
    /// read-only rule is the checker's, not C's). An array value parameter needs
    /// no such trick — C passes the array struct by value, which is already a
    /// copy.
    fn param_is_pointer(&self, pm: &Param) -> bool {
        pm.by_var || self.types.is_record(pm.ty)
    }

    /// The designator form of an expression where it has to be assignable: the
    /// builtins that take a `VAR` argument (`NEW`, `INC`, `PACK`, …).
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
                    // A fixed array passes its element storage, which keeps the
                    // array's extent visible to the C compiler.
                    Type::Array { len, .. } => (format!("({v}).e"), len.to_string()),
                    // An open array is already an element pointer, and its
                    // length is the hidden parameter: forwarding one must not
                    // take the address of the parameter.
                    Type::OpenArray { .. } => (v.clone(), format!("{v}__len")),
                    Type::StringLit(n) => (format!("\"{v}\""), n.to_string()),
                    _ => (v, "0".into()),
                }
            }
        }
    }

    // ---------------------------------------------------- predefined calls

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
                // The allocation goes through the runtime so that the object's
                // own descriptor is stored at its head, as the guard and the
                // type CASE read it, and so that the failure the report
                // specifies (§6.4: the pointer becomes NIL) is the runtime's
                // single answer — including the heap-exhaustion hook the trap
                // matrix uses, which no other route to `malloc` would see.
                let _ = writeln!(out, "    {p} = rt_new(sizeof({bt}), {desc});");
                let _ = writeln!(out, "    if ({p} != NULL) {{");
                // Records *inside* the new object get their descriptors too.
                self.desc_init(base, &format!("(*{p})"), out, "        ");
                let _ = writeln!(out, "    }}");
            }
            "PACK" => {
                // report §10.2: PACK scales x by 2^n, so n is a value.
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
                // Through a helper: written inline, the operand appears three
                // times and a call in it would run twice, possibly with two
                // different results. `ABS` is defined for REAL too and keeps the
                // operand's type (report §10.2), so the helper is chosen by type.
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
                // report §10.1's own `ORD("0")`: a one-character string is a
                // CHAR, and a CHAR must be read unsigned.
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

    // ------------------------------------------------------------ constants

    /// A constant as C, from the checker's folded value (DESIGN §9.2). This is
    /// the only place a constant becomes C, and it cannot disagree with the
    /// folder because it *is* the folder's answer.
    fn const_cvalue(&self, v: &crate::const_eval::Const) -> String {
        use crate::const_eval::Const as K;
        match v {
            K::Int(n) => format!("((int32_t){}u)", *n as u32),
            K::Real(r) => format!("((float){r:?}f)"),
            K::Bool(b) => if *b { "1".into() } else { "0".into() },
            K::Char(c) => format!("((char){}u)", c),
            K::Set(bits) => format!("((uint32_t){}u)", bits),
            K::Str(s) => format!("((const char *)\"{}\"", escape_c(s)) + ")",
        }
    }

    /// A constant as an interface file spells it: Oberon's own notation, since
    /// the reader parses these with the grammar (DESIGN §7).
    fn const_render(v: &crate::const_eval::Const) -> String {
        use crate::const_eval::Const as K;
        match v {
            K::Int(n) => n.to_string(),
            K::Real(r) => format!("{r:?}"),
            K::Bool(b) => if *b { "TRUE".into() } else { "FALSE".into() },
            K::Char(c) => format!("{c:02X}X"),
            K::Str(s) => format!("\"{}\"", s),
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
            ExprKind::Set(_) => match crate::const_eval::eval(e) {
                Ok(crate::const_eval::Const::Set(bits)) => format!("((uint32_t){}u)", bits),
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

    // ------------------------------------------------------- interface text

    /// Render a type the way an interface file spells it (DESIGN §7): the
    /// inverse of `RenderedTypeParser`, so what the writer produces the reader
    /// parses back with the module grammar.
    pub fn render_type(&self, t: TypeId) -> String {
        // A named type is referred to by name: record identity is the
        // declaration, the reader builds a whole interface at once, and
        // inlining a self-referential record would not terminate.
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
                // An anonymous record has to be written out for a client to lay
                // it out; its fields are `;`-separated and carry the public mark
                // (report §6.3), which the reader accepts.
                let mut text = match base {
                    Some(b) => format!("RECORD ({})", self.render_type(*b)),
                    None => "RECORD".to_string(),
                };
                for f in fields {
                    // The mark is the field's own (report §6.3): a client may
                    // name an exported field and nothing else.
                    let mark = if f.export { "*" } else { "" };
                    let _ = write!(text, " {}{mark}: {};", mangle(&f.name), self.render_type(f.ty));
                }
                // Drop the separator after the last field: the reader's
                // `FieldListSequence` is `;`-separated (report §6.3).
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

/// The interface of a checked module: what a client may refer to (DESIGN §7).
/// Only exported objects are written, and an exported variable is read-only by
/// the time a client sees it (report §11).
pub fn interface_of(module: &Module, types: &Types, sema: &Sema<'_>) -> crate::sym::Interface {
    let mut e = Emitter::new(module.name.clone(), types, sema);
    let mut wanted = Vec::new();
    e.collect_types(&module.decls, &mut wanted);
    let mut iface = crate::sym::Interface {
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
            iface.items.push(crate::sym::Item::Const {
                name: c.name.clone(),
                value,
            });
        }
    }
    for t in &module.decls.types {
        if t.export {
            let ty = e.decl_type(&t.ty);
            iface.items.push(crate::sym::Item::Type {
                name: t.name.clone(),
                def: e.render_type_body(ty),
            });
        }
    }
    for v in &module.decls.vars {
        let ty = e.decl_type(&v.ty);
        for (name, export) in &v.names {
            if *export {
                iface.items.push(crate::sym::Item::Var {
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
        let (flat, result) = match sema.signatures.get(&crate::sema::span_key(p.span)) {
            Some(sig) => (sig.params.clone(), sig.result),
            None => (Vec::new(), None),
        };
        iface.items.push(crate::sym::Item::Proc {
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

fn span_of_type(t: &TypeExpr) -> Span {
    match t {
        TypeExpr::Named(_, s)
        | TypeExpr::Array { span: s, .. }
        | TypeExpr::Record { span: s, .. }
        | TypeExpr::Pointer(_, s)
        | TypeExpr::Proc(_, s) => *s,
    }
}

/// An Oberon identifier cannot contain `__`, so every generated name (module
/// prefixes, frames, temporaries) is out of reach of user code.
fn mangle(name: &str) -> String {
    let name = name.replace('.', "__");
    // An Oberon identifier is case-sensitive, so `int`, `while`, `char`, … are
    // legal Oberon names and must not reach C unchanged. Nothing else needs
    // escaping: every generated C name is either `Module__Name` (and `__`
    // cannot occur in an Oberon identifier), a `TD_`/`rt_`/`ob_` runtime name,
    // or already mangled here.
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
