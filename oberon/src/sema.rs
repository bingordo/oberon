//! The semantic pass: scopes, declarations, type resolution and the checks of
//! DESIGN.md §8.5, driven over the syntax AST produced by `oberon.lalrpop`.
//!
//! One walk, the way the reference Oberon compiler works: a scope chain, types
//! built on the fly, diagnostics carrying spans into the only tree there is.
//! The two predicates it leans on are separate and already tested — `types.rs`
//! for the type algebra and `const_eval.rs` for constants — so what remains here
//! is declaration bookkeeping and the checks themselves.
//!
//! Each check names the catalogue row and the report section it comes from.
//! `SELF_HOSTING` (bottom of the file) checks this pass on the report's own
//! modules, and `tests` covers the negative cases row by row.

use std::collections::{HashMap, HashSet};

use crate::ast::{
    self, CaseLabel, Expr, ExprKind, FormalParams, Module, QIdent, Selector, Span, Stmt, StmtKind,
    TypeExpr,
};
use crate::const_eval::{self, Const};
use crate::resolve::{reclassify, Names};
use crate::sym::{Interface, Item};
use crate::types::{Field, Basic, Param, Type, TypeId, Types};

/// A procedure's signature as the checker flattened it (DESIGN §9.7): the
/// procedure's type, one entry per parameter with its mode, and the result.
#[derive(Debug, Clone)]
pub struct Signature {
    pub ty: TypeId,
    pub params: Vec<(String, Param)>,
    pub result: Option<TypeId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub span: Span,
    pub message: String,
    /// A second line of explanation: what the compiler expected, which report
    /// rule applies, or what the mistaken construct probably meant.
    pub notes: Vec<String>,
}

impl Diagnostic {
    pub fn new(severity: Severity, span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            severity,
            span,
            message: message.into(),
            notes: Vec::new(),
        }
    }

    pub fn error(span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic::new(Severity::Error, span, message)
    }

    /// Notes accumulate: a syntax error can have both "what would have fitted
    /// here" and "what the recovery discarded", and the reader needs both
    /// (DESIGN §10).
    pub fn with_note(mut self, note: impl Into<String>) -> Diagnostic {
        self.notes.push(note.into());
        self
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

/// Drop diagnostics that repeat one already reported: two passes over the same
/// mistake (a call looked up twice, say) must not read as two mistakes.
pub fn dedup_diagnostics(diags: &mut Vec<Diagnostic>) {
    let mut seen: Vec<(Severity, u32, u32, String)> = Vec::new();
    diags.retain(|d| {
        let key = (d.severity, d.span.lo, d.span.hi, d.message.clone());
        if seen.contains(&key) {
            false
        } else {
            seen.push(key);
            true
        }
    });
}

/// What an identifier denotes (report §4: an object is a constant, a type, a
/// variable, or a procedure).
#[derive(Debug, Clone)]
enum Sym {
    Const(Const, TypeId),
    Type(TypeId),
    Var {
        ty: TypeId,
        /// `Some(reason)` when the object may not be assigned to.
        read_only: Option<&'static str>,
    },
    Param {
        ty: TypeId,
        by_var: bool,
    },
    Proc {
        ty: TypeId,
        /// A procedure declared inside another procedure; report §6.5 forbids
        /// assigning one to a procedure variable (check 50).
        local: bool,
        /// Identity of a procedure *this compilation unit declares*, so a use
        /// can be attributed to it despite shadowing (check 54's
        /// unused-procedure half). `None` for an imported or predefined one.
        id: Option<usize>,
    },
    /// A predeclared procedure of report §10.2.
    Predef(&'static str),
    /// An imported module; the index selects the interface.
    Module(usize),
}

#[derive(Debug, Default)]
struct Scope {
    names: HashMap<String, Sym>,
}

/// An expression's type plus what the checks need to know about the expression
/// itself.
#[derive(Debug, Clone)]
struct Typed {
    ty: TypeId,
    /// Denotes a variable, so it may be a `VAR` actual or an assignment target.
    lvalue: bool,
    /// May this be passed to a `VAR` parameter? False for a constant and for a
    /// structured value parameter; true for a variable, *including* an imported
    /// one - report §11 forbids assigning to an imported variable, not passing
    /// it, and the report's own `Out` passes `Oberon.Log` to
    /// `Texts.Append(VAR w: Writer; ...)`.
    var_ok: bool,
    read_only: Option<&'static str>,
    /// A `VAR` parameter of record type: this is what makes a guard or `IS`
    /// applicable to a record-typed designator (report §8.1).
    var_record_param: bool,
    /// The assigned name, for the `FOR` control-variable lock.
    name: Option<String>,
    /// A procedure used as a value rather than called.
    proc: Option<&'static str>,
}

impl Typed {
    fn value(ty: TypeId) -> Typed {
        Typed {
            ty,
            lvalue: false,
            var_ok: false,
            read_only: None,
            var_record_param: false,
            name: None,
            proc: None,
        }
    }
    fn error(ty: TypeId) -> Typed {
        Typed::value(ty)
    }
}

pub struct Sema<'a> {
    pub types: Types,
    scopes: Vec<Scope>,
    pub diags: Vec<Diagnostic>,
    interfaces: &'a [Interface],
    /// Control variables of enclosing `FOR` loops (report §9.8).
    locked: Vec<String>,
    /// Names that were looked up, so an unused import can be reported.
    used: Vec<String>,
    /// Procedures this unit declares: id, name, the span to point at, and
    /// whether the declaration is exported (an exported procedure is used by
    /// its clients, so it can never be unused here).
    declared_procs: Vec<(usize, String, Span, bool)>,
    /// Ids of procedures that were used: called, or referenced as a value.
    used_procs: Vec<usize>,
    /// For a name reported as undeclared: the index of that first diagnostic, so
    /// further uses of the same name can be counted onto it instead of repeated.
    /// Per undeclared name: the index of its first diagnostic and how many
    /// further uses were suppressed. Probes that must not report (`lookup`) do
    /// not count, so this is one entry per *use*.
    undeclared: HashMap<String, (usize, usize)>,
    /// Where a constant was used, and what it was worth: `const_at` reads this.
    const_uses: HashMap<u64, Const>,
    /// Record types that came from an interface, and the interface they came
    /// from: naming one of their *unmarked* fields is an error (rows 4 and 14).
    imported_types: HashMap<TypeId, usize>,
    next_proc_id: usize,
    imports: Vec<(String, Span)>,
    /// Built type environments of imported modules, by interface index.
    envs: HashMap<usize, HashMap<String, TypeId>>,
    /// Guards against a cycle while building those environments.
    resolving: Vec<usize>,
    /// While reading a rendered type, the modules the *defining* interface
    /// names: its own plus its import list.
    ctx: Vec<(String, usize)>,
    /// Every procedure's signature, flattened to one entry per parameter and
    /// keyed by the procedure's span. The C emitter needs exactly this: the
    /// procedure's type, the flattened order with its modes and open-array
    /// wrapping, and the result type.
    pub signatures: HashMap<u64, Signature>,
    /// Every constant declaration's folded value and type, keyed by the
    /// declaration's span. The emitter serialises *this*, rather than folding
    /// the syntax again with its own partial rules (which is how `B = A + 1`
    /// once became `0 + 1`).
    pub consts: HashMap<u64, (Const, TypeId)>,
    /// The callee's procedure type for every call the pass typed, keyed by the
    /// designator's span. The emitter needs the *formal parameters* to build the
    /// C argument list, and for an imported procedure that is only available
    /// here.
    pub call_types: HashMap<u64, TypeId>,
    /// The type of every expression the pass typed, keyed by its span. This is
    /// what the C emitter consults instead of re-deriving types (DESIGN §9):
    /// it needs them for the handful of places where Oberon's types are not
    /// C's — a record up-cast, a pointer up/down cast, a string into a `CHAR`
    /// array, a guard, a type-`CASE` narrowing.
    pub types_at: HashMap<u64, TypeId>,
}

impl<'a> Sema<'a> {
    pub fn new(interfaces: &'a [Interface]) -> Sema<'a> {
        Sema {
            types: Types::new(),
            scopes: Vec::new(),
            diags: Vec::new(),
            interfaces,
            locked: Vec::new(),
            used: Vec::new(),
            declared_procs: Vec::new(),
            used_procs: Vec::new(),
            undeclared: HashMap::new(),
            const_uses: HashMap::new(),
            imported_types: HashMap::new(),
            next_proc_id: 0,
            imports: Vec::new(),
            envs: HashMap::new(),
            resolving: Vec::new(),
            ctx: Vec::new(),
            signatures: HashMap::new(),
            consts: HashMap::new(),
            call_types: HashMap::new(),
            types_at: HashMap::new(),
        }
    }

    /// Check one module. The tree is reclassified in place first (D2/D3), so a
    /// caller can inspect the resolved selectors afterwards.
    pub fn check(module: &mut Module, interfaces: &'a [Interface]) -> (Types, Vec<Diagnostic>) {
        let sema = Sema::check_owned(module, interfaces);
        (sema.types, sema.diags)
    }

    /// The same, but handing back the whole pass: the C emitter needs the type
    /// side table as well (DESIGN §9).
    pub fn check_owned(module: &mut Module, interfaces: &'a [Interface]) -> Sema<'a> {
        let mut names = Names::collect(module);
        let mut sema = Sema::new(interfaces);
        if let Err(e) = reclassify(module, &mut names) {
            sema.error(e.span, e.message);
        }
        // The report's outermost scope is the *universe* of predefined
        // identifiers (§10.2), and a module declares its own scope inside it —
        // which is what lets a module or procedure declare `ABS` and have it hide
        // the predefined one (§4). Declaring both in one scope made that a
        // duplicate-declaration error instead.
        sema.push_scope();
        sema.predefine();
        sema.push_scope();
        sema.imports(module);
        sema.decls(&module.decls, true);
        for s in &module.body {
            sema.stmt(s);
        }
        sema.unused_imports(module);
        sema.unused_procedures();
        sema.finish_undeclared_notes();
        dedup_diagnostics(&mut sema.diags);
        sema
    }

    /// The interface index of a module, and its type environment, for the code
    /// generator (DESIGN §9.8: an imported type keeps the defining module's
    /// identity).
    pub fn interface_index(&self, module: &str) -> Option<usize> {
        self.interfaces.iter().position(|i| i.module == module)
    }
    pub fn env_of(&self, idx: usize) -> std::collections::HashMap<String, TypeId> {
        self.envs.get(&idx).cloned().unwrap_or_default()
    }

    /// The folded value of a constant declaration, as the checker computed it.
    pub fn const_of(&self, span: Span) -> Option<&(Const, TypeId)> {
        self.consts.get(&span_key(span))
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diags
    }

    /// The `Module.Name` a type came from, if it came from an interface. The
    /// interface writer needs this: a rendering may only name its own types
    /// unqualified, everything else has to be qualified (DESIGN §7).
    pub fn qualified_name(&self, t: TypeId) -> Option<String> {
        for (idx, env) in &self.envs {
            for (name, ty) in env {
                if *ty == t {
                    return Some(format!("{}.{}", self.interfaces[*idx].module, name));
                }
            }
        }
        None
    }

    // -------------------------------------------------------------- plumbing

    fn note_type(&mut self, span: Span, ty: TypeId) {
        self.types_at.insert(key_of(span), ty);
    }

    /// The type the pass computed for an expression at this span, if any.
    /// For diagnostics and the emitter's debugging.
    pub fn types_at_keys(&self) -> Vec<u64> {
        self.types_at.keys().copied().collect()
    }

    pub fn type_at(&self, span: Span, types: &Types) -> Option<TypeId> {
        self.types_at.get(&key_of(span)).copied().filter(|t| !types.is_error(*t))
    }

    fn error(&mut self, span: Span, message: impl Into<String>) {
        self.diags
            .push(Diagnostic::new(Severity::Error, span, message));
    }
    fn warn(&mut self, span: Span, message: impl Into<String>) {
        self.diags
            .push(Diagnostic::new(Severity::Warning, span, message));
    }
    fn error_note(&mut self, span: Span, message: impl Into<String>, note: impl Into<String>) {
        self.diags
            .push(Diagnostic::error(span, message).with_note(note));
    }
    /// Report and yield the poison type, so one mistake does not cascade.
    fn error_ty(&mut self, span: Span, message: impl Into<String>) -> TypeId {
        let t = self.types.error();
        self.error(span, message);
        t
    }

    fn push_scope(&mut self) {
        self.scopes.push(Scope::default());
    }
    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// ✔ row 2: report §4, "No identifier may denote more than one object within
    /// a given scope."
    fn declare(&mut self, name: &str, sym: Sym, span: Span) {
        let duplicate = self
            .scopes
            .last()
            .expect("a scope is open")
            .names
            .contains_key(name);
        if duplicate {
            self.error(
                span,
                format!("`{name}` is already declared in this scope (report 4)"),
            );
            return;
        }
        self.scopes
            .last_mut()
            .expect("a scope is open")
            .names
            .insert(name.to_string(), sym);
    }

    fn lookup(&self, name: &str) -> Option<Sym> {
        self.scopes
            .iter()
            .rev()
            .find_map(|s| s.names.get(name).cloned())
    }
    fn lookup_here(&self, name: &str) -> Option<Sym> {
        self.scopes.last().and_then(|s| s.names.get(name)).cloned()
    }

    /// The type a qualified name denotes, resolved *without* reporting: the
    /// type-`CASE` arm view asks this question and the label check is what
    /// reports, so asking twice must not say the same thing twice.
    fn type_of_qual_silent(&mut self, q: &QIdent) -> Option<TypeId> {
        if q.len() == 1 {
            return match self.lookup(&q[0]) {
                Some(Sym::Type(t)) => Some(t),
                _ => None,
            };
        }
        if q.len() != 2 {
            return None;
        }
        let idx = match self.lookup(&q[0]) {
            Some(Sym::Module(i)) => i,
            _ => return None,
        };
        let env = self.interface_env(idx, Span::default());
        env.get(&q[1]).copied()
    }

    /// ✔ row 1: report §4, every identifier must be declared or predefined.
    fn lookup_qual(&mut self, q: &QIdent, span: Span) -> Option<Sym> {
        if q.len() == 1 {
            self.used.push(q[0].clone());
            let found = self.lookup(&q[0]);
            if let Some(Sym::Const(c, _)) = &found {
                self.const_uses.insert(span_key(span), c.clone());
            }
            if let Some(Sym::Proc { id: Some(id), .. }) = found {
                // A use of a procedure this unit declares: check 54's
                // unused-procedure half reads this. Qualified names cannot
                // reach a local procedure, so they need no case.
                self.used_procs.push(id);
            }
            return match found {
                Some(s) => Some(s),
                None => {
                    // Report a name once. Later uses of the same undeclared
                    // name add nothing but noise, so they are counted onto the
                    // first diagnostic instead (PLAN.md W5.2).
                    match self.undeclared.get_mut(&q[0]) {
                        Some((_, count)) => *count += 1,
                        None => {
                            let at = self.diags.len();
                            self.error(span, format!("`{}` is not declared (report 4)", q[0]));
                            self.undeclared.insert(q[0].clone(), (at, 0));
                        }
                    }
                    None
                }
            };
        }
        self.used.push(q[0].clone());
        // Inside an interface's own rendering, a qualifier resolves through
        // that interface's import list (or the module itself), not through the
        // importing module's scope.
        let from_ctx = self
            .ctx
            .iter()
            .rev()
            .find(|(name, _)| *name == q[0])
            .map(|(_, idx)| *idx);
        let idx = match from_ctx.or_else(|| match self.lookup(&q[0]) {
            Some(Sym::Module(i)) => Some(i),
            _ => None,
        }) {
            // ✔ row 3: a qualifier must be a module from the import list.
            Some(i) => i,
            None => {
                match self.lookup(&q[0]) {
                    Some(_) => self.error(
                        span,
                        format!("`{}` is not a module, so it cannot qualify a name (report 11)", q[0]),
                    ),
                    None => self.error(
                        span,
                        format!(
                            "`{}` is not a declared name or a module with an interface, so `{}.{}` \
                             cannot be resolved (report 11)",
                            q[0], q[0], q[1]
                        ),
                    ),
                }
                return None;
            }
        };
        let module = self.interfaces[idx].module.clone();
        // ✔ row 4: the object must be exported by that module.
        let found = self.interfaces[idx]
            .items
            .iter()
            .find(|i| item_name(i) == q[1])
            .cloned();
        match found {
            Some(it) => self.imported_sym(idx, &it, span),
            None => {
                self.error(
                    span,
                    format!("`{}.{}` is not exported by module `{module}` (report 11)", q[0], q[1]),
                );
                None
            }
        }
    }

    fn predefine(&mut self) {
        for (name, b) in [
            ("BOOLEAN", Basic::Boolean),
            ("CHAR", Basic::Char),
            ("INTEGER", Basic::Integer),
            ("REAL", Basic::Real),
            ("BYTE", Basic::Byte),
            ("SET", Basic::Set),
        ] {
            let ty = self.types.basic(b);
            self.declare(name, Sym::Type(ty), Span::default());
        }
        for name in PREDEFINED_FUNCTIONS {
            self.declare(name, Sym::Predef(name), Span::default());
        }
        for name in PREDEFINED_PROPER {
            self.declare(name, Sym::Predef(name), Span::default());
        }
    }

    // --------------------------------------------------------------- imports

    fn imports(&mut self, m: &Module) {
        for imp in &m.imports {
            // ✔ row 53: report §11.1's module is optional and not provided.
            if imp.module == "SYSTEM" {
                self.error(
                    imp.span,
                    "module SYSTEM is not available in this implementation (report 11.1)",
                );
                continue;
            }
            // ✔ row 5: no self-import, no duplicate alias (row 2 covers the
            // duplicate through `declare`).
            if imp.alias == m.name {
                self.error(
                    imp.span,
                    format!("module `{}` cannot import itself (report 11)", m.name),
                );
                continue;
            }
            match self.interfaces.iter().position(|i| i.module == imp.module) {
                Some(idx) => {
                    self.imports.push((imp.alias.clone(), imp.span));
                    self.declare(&imp.alias, Sym::Module(idx), imp.span);
                }
                None => self.error(
                    imp.span,
                    format!(
                        "no interface for module `{}`: compile it first (report 11)",
                        imp.module
                    ),
                ),
            }
        }
    }

    /// The value of a constant at the place it is used, so the emitter can fold
    /// an expression the checker has already resolved the names in (R20).
    pub fn const_at(&self, span: Span) -> Option<Const> {
        self.const_uses.get(&span_key(span)).cloned()
    }

    /// Attaches `N further uses are suppressed` to each undeclared name's first
    /// diagnostic. Called once, at the end.
    fn finish_undeclared_notes(&mut self) {
        let entries: Vec<(String, (usize, usize))> = self
            .undeclared
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        for (name, (at, count)) in entries {
            if count == 0 || at >= self.diags.len() {
                continue;
            }
            let msg = if count == 1 {
                format!("1 further use of `{name}` is suppressed")
            } else {
                format!("{count} further uses of `{name}` are suppressed")
            };
            self.diags[at].notes.push(msg);
        }
    }

    /// ✔ row 54 (the other half): a procedure this unit declares and never uses
    /// is worth a warning, since the report has no way to mark one as
    /// deliberately unused. A procedure that only calls *itself* counts as used,
    /// which is a known imprecision: the report has no way to say "not
    /// reachable", and reachability is not what this checks.
    fn unused_procedures(&mut self) {
        let declared = self.declared_procs.clone();
        for (id, name, span, exported) in declared {
            if exported || self.used_procs.contains(&id) {
                continue;
            }
            self.warn(span, format!("procedure `{name}` is never used"));
        }
    }

    fn unused_imports(&mut self, m: &Module) {
        // ✔ row 54 (warning half): an import that is never qualified.
        let imports = self.imports.clone();
        for (alias, span) in imports {
            let used = self.used.iter().any(|n| n == &alias);
            if !used && m.imports.iter().any(|i| i.alias == alias) {
                self.warn(span, format!("import `{alias}` is never used"));
            }
        }
    }

    /// The symbol an interface item denotes in the importing module. `idx` is
    /// the interface's index, so its type environment can be consulted.
    fn imported_sym(&mut self, idx: usize, it: &Item, span: Span) -> Option<Sym> {
        let env = self.interface_env(idx, span);
        match it {
            Item::Const { name, value } => match parse_const_text(value) {
                Some(c) => {
                    let t = self.const_type(&c);
                    Some(Sym::Const(c, t))
                }
                None => {
                    self.error(
                        span,
                        format!(
                            "the interface value `{value}` of constant `{name}` is not in a form \
                             this implementation reads"
                        ),
                    );
                    None
                }
            },
            Item::Type { name, .. } => match env.get(name) {
                Some(t) => Some(Sym::Type(*t)),
                None => Some(Sym::Type(self.types.error())),
            },
            Item::Var { ty, .. } => Some(Sym::Var {
                ty: self.type_from_rendered(ty, &env, idx, span),
                // report §11: "Variables are always exported in read-only mode."
                read_only: Some("is an imported variable, which is read-only (report 11)"),
            }),
            Item::Proc { params, result, .. } => {
                let mut ps = Vec::new();
                for p in params {
                    let (by_var, rest) = match p.strip_prefix("VAR ") {
                        Some(rest) => (true, rest),
                        None => (false, p.as_str()),
                    };
                    let ty = match rest.split_once(": ") {
                        Some((_, ty)) => ty,
                        None => rest,
                    };
                    let t = self.type_from_rendered(ty, &env, idx, span);
                    ps.push(Param { by_var, ty: t });
                }
                let r = result
                    .as_deref()
                    .map(|r| self.type_from_rendered(r, &env, idx, span));
                Some(Sym::Proc {
                    ty: self.types.proc(ps, r),
                    local: false,
                    id: None,
                })
            }
        }
    }

    /// The type environment of an imported module, built from its interface
    /// (DESIGN §7). Rendered types are parsed with the very production the
    /// module parser uses, and their names resolve inside the interface's own
    /// module — so `Texts.Writer = RECORD buf: Buffer END` gives the importer
    /// the real record, and a cycle between two interfaces terminates because
    /// the record placeholders exist before any body is built.
    fn interface_env(&mut self, idx: usize, span: Span) -> HashMap<String, TypeId> {
        if let Some(e) = self.envs.get(&idx) {
            return e.clone();
        }
        if self.resolving.contains(&idx) {
            return HashMap::new();
        }
        self.resolving.push(idx);
        let mut decls: Vec<(String, TypeExpr)> = Vec::new();
        let mut unreadable: Vec<String> = Vec::new();
        for i in &self.interfaces[idx].items {
            if let Item::Type { name, def } = i {
                match rendered_type(def) {
                    Some(t) => decls.push((name.clone(), t)),
                    // Skipping it silently is how a client ends up being told
                    // "not declared" about a name the interface does export.
                    None => unreadable.push(format!("{name} = {def}")),
                }
            }
        }
        let env = self.build_type_group(&decls, false);
        // Every type this interface declares is *imported* as far as a field's
        // visibility is concerned (rows 4 and 14): naming one of its unmarked
        // fields is an error. The types are built by the ordinary declaration
        // path, so the marks go on here rather than inside the builder.
        for t in env.values() {
            self.mark_imported(*t, idx);
        }
        self.resolving.pop();
        if !unreadable.is_empty() {
            // A type the interface declares but we cannot read is a build
            // error, not a missing name: say so where the client first refers
            // to this module. The environment is built once, so this is said
            // once.
            let module = self.interfaces[idx].module.clone();
            self.error(
                span,
                format!(
                    "the interface of `{module}` declares {} this implementation cannot read: {}",
                    if unreadable.len() == 1 { "a type" } else { "types" },
                    unreadable.join(", ")
                ),
            );
        }
        self.envs.insert(idx, env.clone());
        env
    }

    /// Build a type written in an interface file. `module` is the module the
    /// interface belongs to, for its own qualified references. A formal type is
    /// `{ARRAY OF} qualident` (report §10.1), so the open-array prefix is peeled
    /// off here rather than being part of the type grammar.
    fn type_from_rendered(
        &mut self,
        text: &str,
        env: &HashMap<String, TypeId>,
        module_idx: usize,
        span: Span,
    ) -> TypeId {
        let mut opens = 0usize;
        let mut rest = text.trim();
        while let Some(r) = rest.strip_prefix("ARRAY OF ") {
            opens += 1;
            rest = r.trim();
        }
        let t = match rendered_type(rest) {
            Some(t) => t,
            None => {
                return self.error_ty(
                    span,
                    format!("the interface's rendering `{text}` is not a type this implementation parses"),
                )
            }
        };
        // The defining interface's own name and import list, so a qualified
        // type in it resolves where it was written.
        let base = self.ctx.len();
        self.ctx.push((
            self.interfaces[module_idx].module.clone(),
            module_idx,
        ));
        for name in self.interfaces[module_idx].imports.clone() {
            if let Some(i) = self.interfaces.iter().position(|m| m.module == name) {
                self.ctx.push((name, i));
            }
        }
        let mut ts = TypeScope {
            placeholders: env.clone(),
            names: env.keys().cloned().collect(),
            defined: env.keys().cloned().collect(),
            result: env.clone(),
        };
        let mut built = self.type_expr(&t, &mut ts, false);
        for _ in 0..opens {
            built = self.types.open_array(built);
        }
        self.ctx.truncate(base);
        // Everything this interface declares is *imported* as far as a field's
        // visibility is concerned: naming one of its unmarked fields is an error
        // (rows 4 and 14).
        self.mark_imported(built, module_idx);
        built
    }

    /// Records every record reachable from a type that came from an interface,
    /// with the interface it came from, so a field access through one can be
    /// checked against the field's public mark.
    fn mark_imported(&mut self, t: TypeId, module_idx: usize) {
        // A recursive type (`POINTER TO Node` inside `Node`) would otherwise spin
        // here until a depth guard cut the walk off, leaving the records it had
        // not reached yet unmarked — a small legal type, not a deep input.
        let mut seen: HashSet<TypeId> = HashSet::new();
        let mut stack = vec![t];
        while let Some(t) = stack.pop() {
            if !seen.insert(t) {
                continue;
            }
            match self.types.get(t).clone() {
                Type::Record { base, fields, .. } => {
                    self.imported_types.insert(t, module_idx);
                    for f in &fields {
                        stack.push(f.ty);
                    }
                    if let Some(b) = base {
                        stack.push(b);
                    }
                }
                Type::Pointer { base } => stack.push(base),
                Type::Array { elem, .. } | Type::OpenArray { elem } => stack.push(elem),
                _ => {}
            }
        }
    }

    // ---------------------------------------------------------- declarations

    fn decls(&mut self, d: &ast::Decls, is_module: bool) {
        for c in &d.consts {
            // ✔ row 48: export marks only at module scope.
            self.check_export_mark(c.export, is_module, c.span);
            self.const_decl(c);
        }
        self.type_section(&d.types, is_module);
        for v in &d.vars {
            self.var_decl(v, is_module);
        }
        for p in &d.procs {
            self.proc_decl(p, is_module);
        }
    }

    fn check_export_mark(&mut self, export: bool, is_module: bool, span: Span) {
        if export && !is_module {
            self.error(
                span,
                "an export mark is only allowed at module scope (report 4)",
            );
        }
    }

    /// ✔ row 40: report §5, the value must be a constant expression.
    fn const_decl(&mut self, c: &ast::ConstDecl) {
        if self.lookup_here(&c.name).is_some() {
            return; // the duplicate was already reported
        }
        match self.const_expr(&c.value) {
            Ok(v) => {
                let ty = self.const_type(&v);
                self.consts.insert(key_of(c.span), (v.clone(), ty));
                self.declare(&c.name, Sym::Const(v, ty), c.span);
            }
            Err(msg) => {
                let ty = self.error_ty(c.value.span, format!("constant `{}`: {msg}", c.name));
                self.declare(&c.name, Sym::Const(Const::Int(0), ty), c.span);
            }
        }
    }

    /// Fold a constant expression (report §5). `const_eval` works on syntax
    /// alone, so first replace what only the symbol table can resolve — a named
    /// constant, and `LEN(v)` of a declared array (report §10.2 makes `LEN` of
    /// an array a constant) — with literals, then fold.
    fn const_expr(&mut self, e: &Expr) -> Result<Const, String> {
        let mut e = e.clone();
        self.substitute(&mut e);
        const_eval::eval(&e)
    }

    /// Rewrite the resolvable parts of a constant expression in place.
    fn substitute(&mut self, e: &mut Expr) {
        match &mut e.kind {
            ExprKind::Designator(d) => {
                if d.base.len() != 1 {
                    return;
                }
                let name = d.base[0].clone();
                if d.sels.is_empty() {
                    if let Some(Sym::Const(v, _)) = self.lookup(&name) {
                        e.kind = literal(&v);
                        return;
                    }
                }
                // `LEN(v)` of a fixed-size array.
                if name == "LEN" {
                    if let [Selector::Call(args)] = d.sels.as_slice() {
                        if args.len() == 1 {
                            if let ExprKind::Designator(inner) = &args[0].kind {
                                if inner.base.len() == 1 && inner.sels.is_empty() {
                                    if let Some(Sym::Var { ty, .. } | Sym::Param { ty, .. }) =
                                        self.lookup(&inner.base[0])
                                    {
                                        if let Some(n) = self.types.array_len(ty) {
                                            self.used.push(inner.base[0].clone());
                                            e.kind = ExprKind::Int(n);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            ExprKind::Unary(_, x) => self.substitute(x),
            ExprKind::Binary(_, l, r) => {
                self.substitute(l);
                self.substitute(r);
            }
            ExprKind::Set(elems) => {
                for el in elems {
                    self.substitute(&mut el.lo);
                    if let Some(h) = &mut el.hi {
                        self.substitute(h);
                    }
                }
            }
            ExprKind::TypeTest(x, _) => self.substitute(x),
            _ => {}
        }
    }

    fn const_type(&mut self, v: &Const) -> TypeId {
        match v {
            Const::Int(_) => self.types.integer(),
            Const::Real(_) => self.types.real(),
            Const::Bool(_) => self.types.boolean(),
            Const::Char(_) => self.types.char(),
            Const::Str(s) => self.types.string_lit(s.len()),
            Const::Set(_) => self.types.set(),
        }
    }

    /// Types in declaration order.
    ///
    /// The report allows exactly one forward reference inside a scope — a
    /// pointer's base type (§6.4) — so a declaration whose body *is* a record
    /// gets a placeholder that is later defined **in place**, which is what
    /// makes `Tree = POINTER TO Node; Node = RECORD … END` give `Tree` a pointer
    /// to the filled-in record rather than to an empty shell. Every other name
    /// is declared only after its body is built, so referencing it earlier is
    /// reported (with the forward-reference rule as the note).
    fn type_section(&mut self, decls: &[ast::TypeDecl], is_module: bool) {
        for t in decls {
            self.check_export_mark(t.export, is_module, span_of_type(&t.ty));
        }
        let pairs: Vec<(String, TypeExpr)> = decls
            .iter()
            .map(|t| (t.name.clone(), t.ty.clone()))
            .collect();
        self.build_type_group(&pairs, true);
    }

    /// Build a group of mutually visible type declarations. With
    /// `declare_in_scope` the names are put into the current scope; otherwise
    /// the group is an imported module's environment and only the returned map
    /// is used.
    fn build_type_group(
        &mut self,
        decls: &[(String, TypeExpr)],
        declare_in_scope: bool,
    ) -> HashMap<String, TypeId> {
        let mut ts = TypeScope::default();
        for (name, ty) in decls {
            ts.names.insert(name.clone());
            if matches!(ty, TypeExpr::Record { .. }) {
                let ph = self.types.record_placeholder();
                ts.placeholders.insert(name.clone(), ph);
                if declare_in_scope {
                    self.declare(name, Sym::Type(ph), span_of_type(ty));
                }
            }
        }
        for (name, ty) in decls {
            let built = if let TypeExpr::Record { base, fields, span } = ty {
                let ph = ts.placeholders[name];
                let (b, fs) = self.record_body(base, fields, *span, &mut ts);
                self.types.define_record(ph, b, fs);
                // New decision (DESIGN §13): report §6.3 permits a field to hide
                // an inherited field of the same name, and it stays legal — but
                // `x.f` then means the derived field silently, so say so.
                if let Some(bt) = b {
                    let mut inherited: Vec<String> = Vec::new();
                    let mut walk = Some(bt);
                    let mut guard = 0;
                    while let Some(t) = walk {
                        guard += 1;
                        if guard > 64 {
                            break;
                        }
                        match self.types.get(t) {
                            Type::Record { base, fields, .. } => {
                                for f in fields {
                                    inherited.push(f.name.clone());
                                }
                                walk = *base;
                            }
                            _ => break,
                        }
                    }
                    for f in fields {
                        for (fname, _) in &f.names {
                            if inherited.iter().any(|n| n == fname) {
                                self.warn(
                                    f.span,
                                    format!(
                                        "field `{fname}` hides a field of the base record; `x.{fname}` means this one (report 6.3)"
                                    ),
                                );
                            }
                        }
                    }
                }
                // A record body is built here rather than through `type_expr`,
                // so its span has to be recorded explicitly: the emitter
                // resolves declarations through this table.
                self.note_type(span_of_type(ty), ph);
                ph
            } else {
                let built = self.type_expr(ty, &mut ts, false);
                if declare_in_scope {
                    self.declare(name, Sym::Type(built), span_of_type(ty));
                }
                built
            };
            ts.defined.insert(name.clone());
            ts.result.insert(name.clone(), built);
        }
        ts.placeholders.extend(ts.result.clone());
        ts.placeholders
    }

    /// `in_pointer`: a name that is still a placeholder may be used as a
    /// pointer's base type (report §6.4) and nowhere else.
    fn type_expr(&mut self, t: &TypeExpr, ts: &mut TypeScope, in_pointer: bool) -> TypeId {
        let built = self.type_expr_inner(t, ts, in_pointer);
        self.note_type(span_of_type(t), built);
        built
    }

    fn type_expr_inner(&mut self, t: &TypeExpr, ts: &mut TypeScope, in_pointer: bool) -> TypeId {
        match t {
            TypeExpr::Named(q, span) => {
                if q.len() == 1 {
                    if let (Some(ph), false) = (ts.placeholders.get(&q[0]), ts.defined.contains(&q[0])) {
                        // A record declared later in this scope: legal exactly
                        // when it is a pointer's base type.
                        if !in_pointer {
                            return self.error_ty(
                                *span,
                                format!(
                                    "`{}` is declared later in this scope; only a POINTER base \
                                     type may be referenced before its declaration (report 6.4)",
                                    q[0]
                                ),
                            );
                        }
                        return *ph;
                    }
                    if let (Some(t), true) = (ts.result.get(&q[0]), ts.defined.contains(&q[0])) {
                        return *t;
                    }
                    if ts.names.contains(&q[0]) && self.lookup(&q[0]).is_none() {
                        return self.error_ty(
                            *span,
                            format!(
                                "`{}` is declared later in this scope and can only be referenced \
                                 before its declaration as a POINTER base type (report 6.4)",
                                q[0]
                            ),
                        );
                    }
                }
                match self.lookup_qual(q, *span) {
                    Some(Sym::Type(ty)) => ty,
                    Some(_) => {
                        self.error_ty(*span, format!("`{}` is not a type (report 6)", q.join(".")))
                    }
                    None => self.types.error(),
                }
            }
            TypeExpr::Array { dims, elem, .. } => {
                // report §6.2: `ARRAY N0, N1, ... OF T` is right-nested.
                let mut ety = self.type_expr(elem, ts, false);
                for dim in dims.iter().rev() {
                    let len = self.array_length(dim);
                    ety = self.types.array(len, ety);
                }
                ety
            }
            TypeExpr::Record { base, fields, span } => {
                let (b, fs) = self.record_body(base, fields, *span, ts);
                self.types.record(b, fs)
            }
            TypeExpr::Pointer(inner, span) => {
                let base = self.type_expr(inner, ts, true);
                // ✔ row 44.
                if !self.types.is_record(base) && !self.types.is_error(base) {
                    self.error(*span, "the base type of a pointer must be a record (report 6.4)");
                }
                self.types.pointer(base)
            }
            TypeExpr::Proc(fp, span) => {
                let (params, result) = self.formals(fp, *span);
                if let Some(r) = result {
                    // ✔ row 45.
                    if self.is_structured(r) {
                        self.error(
                            *span,
                            "the result type of a procedure may not be a record or an array \
                             (report 10.1)",
                        );
                    }
                }
                self.types.proc(params, result)
            }
        }
    }

    /// The base type and fields of a record, with checks 42 and 43.
    fn record_body(
        &mut self,
        base: &Option<QIdent>,
        fields: &[ast::FieldList],
        span: Span,
        ts: &mut TypeScope,
    ) -> (Option<TypeId>, Vec<Field>) {
        let b = match base {
            Some(q) => {
                let bt = self.type_expr(&TypeExpr::Named(q.clone(), span), ts, false);
                // ✔ row 42.
                if !self.types.is_record(bt) && !self.types.is_error(bt) {
                    self.error(span, "the base type of a record must be a record (report 6.3)");
                }
                Some(bt)
            }
            None => None,
        };
        let mut seen: Vec<String> = Vec::new();
        let mut fs: Vec<Field> = Vec::new();
        for fl in fields {
            let fty = self.type_expr(&fl.ty, ts, false);
            for (name, mark) in &fl.names {
                // A field's `*` marks it *public* (report §6.3: "field
                // identifiers that are to be visible outside the declaring
                // module must be marked"). The mark travels with the field, so a
                // later access through a type from an interface can tell whether
                // it may name the field (rows 4 and 14).
                // ✔ row 43.
                if seen.contains(name) {
                    self.error(fl.span, format!("field `{name}` is declared twice (report 6.3)"));
                    continue;
                }
                seen.push(name.clone());
                fs.push(Field {
                    name: name.clone(),
                    ty: fty,
                    export: *mark,
                });
            }
        }
        (b, fs)
    }

    /// ✔ row 41: report §6.2, an array length is a constant expression, and a
    /// length of zero or less has no elements to index.
    fn array_length(&mut self, e: &Expr) -> u32 {
        match self.const_expr(e) {
            Ok(Const::Int(n)) if n > 0 => n as u32,
            Ok(Const::Int(_)) => {
                self.error(e.span, "an array length must be positive (report 6.2)");
                1
            }
            Ok(_) => {
                self.error(e.span, "an array length must be an integer constant (report 6.2)");
                1
            }
            Err(msg) => {
                self.error(e.span, msg);
                1
            }
        }
    }

    fn var_decl(&mut self, v: &ast::VarDecl, is_module: bool) {
        let ty = self.type_expr(&v.ty, &mut TypeScope::default(), false);
        for (name, mark) in &v.names {
            self.check_export_mark(*mark, is_module, v.span);
            self.declare(
                name,
                Sym::Var {
                    ty,
                    read_only: None,
                },
                v.span,
            );
        }
    }

    /// The formal parameters of a procedure heading or a procedure type.
    /// Returns one entry per *name*, because report §10.1 lets one `FPSection`
    /// declare several parameters sharing a type.
    fn formals(&mut self, fp: &FormalParams, span: Span) -> (Vec<Param>, Option<TypeId>) {
        let mut params = Vec::new();
        for section in &fp.params {
            let mut base = self.type_expr(
                &TypeExpr::Named(section.ty.base.clone(), span),
                &mut TypeScope::default(),
                false,
            );
            for _ in 0..section.ty.open {
                base = self.types.open_array(base);
            }
            self.note_type(span_of_type(&TypeExpr::Named(section.ty.base.clone(), span)), base);
            for (name, mark) in &section.names {
                // ✔ row 48, formal-parameter half.
                if *mark {
                    self.error(
                        span,
                        format!("a formal parameter may not carry an export mark (`{name}`)"),
                    );
                }
                params.push(Param {
                    by_var: section.by_ref,
                    ty: base,
                });
            }
        }
        let result = match &fp.result {
            None => None,
            Some(q) => match self.lookup_qual(q, span) {
                Some(Sym::Type(t)) => Some(t),
                Some(_) => Some(self.error_ty(span, format!("`{}` is not a type (report 10)", q.join(".")))),
                None => Some(self.types.error()),
            },
        };
        (params, result)
    }

    fn proc_decl(&mut self, p: &ast::Proc, is_module: bool) {
        self.check_export_mark(p.export, is_module, p.span);
        // The name is declared before the body is checked, which is what makes
        // recursion work (report §10).
        let (declared, result) = self.named_formals(&p.params, p.result.as_ref(), p.span);
        let pty = self.types.proc(
            declared.iter().map(|(_, pm)| *pm).collect(),
            result,
        );
        let id = self.next_proc_id;
        self.next_proc_id += 1;
        self.declared_procs
            .push((id, p.name.clone(), p.name_span, p.export));
        self.declare(
            &p.name,
            Sym::Proc {
                ty: pty,
                local: !is_module,
                id: Some(id),
            },
            p.span,
        );
        self.signatures.insert(
            span_key(p.span),
            Signature {
                ty: pty,
                params: declared.clone(),
                result,
            },
        );
        // ✔ row 47.
        if p.end_name != p.name {
            self.error(
                p.span,
                format!(
                    "the identifier after END is `{}`, but the procedure is `{}` (report 10)",
                    p.end_name, p.name
                ),
            );
        }
        self.push_scope();
        for (name, pm) in &declared {
            self.declare(
                name,
                Sym::Param {
                    ty: pm.ty,
                    by_var: pm.by_var,
                },
                p.span,
            );
        }
        self.decls(&p.decls, false);
        for s in &p.body {
            self.stmt(s);
        }
        // ✔ row 46: a function must end with RETURN; a proper procedure must not.
        match (result, &p.ret) {
            (Some(want), Some(e)) => {
                let got = self.expr(e);
                if !self.types.is_assignable(want, got.ty) {
                    self.error(
                        e.span,
                        "the type of the RETURN expression does not match the result type \
                         (report 10)",
                    );
                }
            }
            (Some(_), None) => self.error(
                p.span,
                format!(
                    "function procedure `{}` must end with a RETURN clause (report 10)",
                    p.name
                ),
            ),
            (None, Some(e)) => self.error(
                e.span,
                format!(
                    "`{}` is a proper procedure: it may not return a value (report 10)",
                    p.name
                ),
            ),
            (None, None) => {}
        }
        self.pop_scope();
    }

    /// Flatten `FPSection`s into one `(name, Param)` per parameter, so the
    /// scope holds one symbol per name (report §10.1).
    fn named_formals(
        &mut self,
        sections: &[ast::Param],
        result: Option<&QIdent>,
        span: Span,
    ) -> (Vec<(String, Param)>, Option<TypeId>) {
        let mut fp = FormalParams {
            params: sections.to_vec(),
            result: result.cloned(),
        };
        // `formals` wants the flattened order to line up with the flat names.
        let (flat, res) = self.formals(&fp, span);
        let mut named: Vec<(String, Param)> = Vec::new();
        let mut i = 0;
        for section in sections {
            for (name, _) in &section.names {
                named.push((name.clone(), flat[i]));
                i += 1;
            }
        }
        fp.params.clear();
        (named, res)
    }

    fn is_structured(&self, t: TypeId) -> bool {
        matches!(
            self.types.get(t),
            Type::Array { .. } | Type::Record { .. } | Type::OpenArray { .. }
        )
    }

    // ---------------------------------------------------------- expressions

    fn expr(&mut self, e: &Expr) -> Typed {
        let v = self.expr_inner(e);
        self.note_type(e.span, v.ty);
        v
    }

    fn expr_inner(&mut self, e: &Expr) -> Typed {
        match &e.kind {
            ExprKind::Int(_) => Typed::value(self.types.integer()),
            ExprKind::Real(_) => Typed::value(self.types.real()),
            ExprKind::Char(_) => Typed::value(self.types.char()),
            ExprKind::Str(s) => Typed::value(self.types.string_lit(s.len())),
            ExprKind::Bool(_) => Typed::value(self.types.boolean()),
            ExprKind::Nil => Typed::value(self.types.nil()),
            ExprKind::Set(elems) => {
                for el in elems {
                    let lo = self.expr(&el.lo);
                    self.expect_integer(el.lo.span, lo.ty, "a set element");
                    self.check_set_element(&el.lo);
                    if let Some(h) = &el.hi {
                        let hi = self.expr(h);
                        self.expect_integer(h.span, hi.ty, "a set range bound");
                        self.check_set_element(h);
                    }
                }
                Typed::value(self.types.set())
            }
            ExprKind::Designator(d) => self.designator(d),
            ExprKind::Unary(op, x) => {
                let v = self.expr(x);
                // ✔ rows 18-21: the operand rules live in `types.rs`.
                if let Err(msg) = self.types.unary_ok(*op, v.ty) {
                    self.error(e.span, msg);
                }
                let ty = match op {
                    ast::UnOp::Not => self.types.boolean(),
                    _ => v.ty,
                };
                Typed::value(ty)
            }
            ExprKind::Binary(op, l, r) => {
                let a = self.expr(l);
                let b = self.expr(r);
                if let Err(msg) = self.types.operator_ok(*op, a.ty, b.ty) {
                    self.error(e.span, msg);
                }
                // report §8.2: relations yield BOOLEAN, the rest keep the
                // operands' type.
                let ty = match op {
                    ast::BinOp::Or
                    | ast::BinOp::And
                    | ast::BinOp::Eq
                    | ast::BinOp::Ne
                    | ast::BinOp::Lt
                    | ast::BinOp::Le
                    | ast::BinOp::Gt
                    | ast::BinOp::Ge
                    | ast::BinOp::In => self.types.boolean(),
                    _ => a.ty,
                };
                Typed::value(ty)
            }
            ExprKind::TypeTest(x, q) => {
                let v = self.expr(x);
                let tested = match self.lookup_qual(q, e.span) {
                    Some(Sym::Type(t)) => t,
                    Some(_) => self.error_ty(e.span, format!("`{}` is not a type (report 8.2.4)", q.join("."))),
                    None => self.types.error(),
                };
                self.guard_type(e.span, &v, tested, "IS");
                Typed::value(self.types.boolean())
            }
        }
    }

    /// ✔ row 52: the elements of a set are the integers 0..31 in this
    /// implementation (report §6.1 leaves the limit to the implementation).
    fn check_set_element(&mut self, e: &Expr) {
        if let Ok(Const::Int(n)) = self.const_expr(e) {
            if !(0..32).contains(&n) {
                self.error(
                    e.span,
                    format!("set element {n} is outside 0..31 (report 6.1, 8.2)"),
                );
            }
        }
    }

    fn expect_integer(&mut self, span: Span, ty: TypeId, what: &str) {
        // ✔ rows 11 and 23.
        if !self.types.is_integer_like(ty) && !self.types.is_error(ty) {
            self.error(span, format!("{what} must be an INTEGER (report 8.1)"));
        }
    }

    /// ✔ rows 15-17, 37 and 38: the applicability of `v IS T`, of a guard
    /// `v(T)` and of a type `CASE` label, and the type the guard yields.
    ///
    /// The report states the rule as "T is an extension of the declared type T0
    /// of v" (report §8.1, §8.2.4) but its own examples spell T two ways for a
    /// pointer v: §8.1 writes `t(CenterNode)` (a *record* name, CenterNode being
    /// an extension of the pointer's base type Node) while §9.5 writes
    /// `CASE p OF P0` (a *pointer* name extending p's pointer type). Both are
    /// accepted here; the guard's type is the type named, which is what makes
    /// §8.1's `t(CenterNode).subnode (Tree)` come out as Tree and my
    /// `c := t(CenterNode)` for `c: CenterNode` type-check.
    fn guard_type(&mut self, span: Span, v: &Typed, named: TypeId, what: &str) -> Option<TypeId> {
        if self.types.is_error(v.ty) || self.types.is_error(named) {
            return Some(self.types.error());
        }
        if self.types.is_pointer(v.ty) {
            let base = self.types.deref(v.ty).expect("checked");
            let record_spelling = self.types.extends(named, base);
            let pointer_spelling = self.types.is_pointer(named) && self.types.extends(named, v.ty);
            if record_spelling || pointer_spelling {
                return Some(named);
            }
            self.error(
                span,
                format!(
                    "the type named by `{what}` must be an extension of `{}` (report 8.1)",
                    self.describe(base)
                ),
            );
            return None;
        }
        if v.var_record_param && self.types.is_record(v.ty) {
            if self.types.extends(named, v.ty) {
                return Some(named);
            }
            self.error(
                span,
                format!(
                    "the type named by `{what}` must be an extension of `{}` (report 8.1)",
                    self.describe(v.ty)
                ),
            );
            return None;
        }
        self.error(
            span,
            format!(
                "`{what}` applies to a pointer or a VAR record parameter, but this designator is \
                 {} (report 8.1)",
                self.describe(v.ty)
            ),
        );
        None
    }

    /// Follow a designator's selectors. `report` rows are marked per step.
    fn designator(&mut self, d: &ast::Designator) -> Typed {
        let v = self.designator_inner(d);
        self.note_type(d.span, v.ty);
        v
    }

    fn designator_inner(&mut self, d: &ast::Designator) -> Typed {
        let mut cur = match self.lookup_qual(&d.base, d.span) {
            Some(s) => s,
            None => return Typed::error(self.types.error()),
        };
        let mut name = d.base.join(".");
        let mut var_record_param = false;
        // Carried through selection: assigning to a field or an element of a
        // read-only object is assigning to that object (report §9.1).
        let read_only: Option<&'static str> = match &cur {
            Sym::Var { read_only, .. } => *read_only,
            Sym::Param { ty, by_var } => {
                if *by_var || !self.is_structured(*ty) {
                    None
                } else {
                    Some("is a structured value parameter, which is read-only (report 10.1)")
                }
            }
            Sym::Const(..) => Some("is a constant, which is read-only (report 9.1)"),
            _ => None,
        };

        for (i, sel) in d.sels.iter().enumerate() {
            match sel {
                Selector::Call(args) => {
                    // ✔ row 29: `f(x)(y)` - a call's result is not a designator.
                    if i + 1 != d.sels.len() {
                        self.error(
                            d.span,
                            "a designator cannot be applied twice: the result of a call cannot be \
                             selected from (report 8.1)",
                        );
                    }
                    return match cur.clone() {
                        Sym::Proc { ty, .. } => {
                            let (params, result) = self.signature(ty);
                            self.check_actuals(d.span, &params, args);
                            self.call_types.insert(key_of(d.span), ty);
                            Typed::value(result.unwrap_or_else(|| self.types.error()))
                        }
                        Sym::Var { ty, .. } if self.types.is_proc(ty) => {
                            let (params, result) = self.signature(ty);
                            self.check_actuals(d.span, &params, args);
                            self.call_types.insert(key_of(d.span), ty);
                            Typed::value(result.unwrap_or_else(|| self.types.error()))
                        }
                        Sym::Predef(n) => self.predef_call(d.span, n, args),
                        _ => {
                            // ✔ row 28.
                            let ty = self.error_ty(
                                d.span,
                                format!("`{name}` is not a procedure, so it cannot be called (report 9.2)"),
                            );
                            Typed::error(ty)
                        }
                    };
                }
                Selector::Index(ix) => {
                    let t = self.sym_type(&cur);
                    match self.types.elem(t) {
                        Some(elem) => {
                            // ✔ row 10: one index per dimension (report §8.1).
                            let dims = self.array_dims(t);
                            if ix.len() != dims {
                                self.error(
                                    d.span,
                                    format!(
                                        "`{name}` has {dims} dimension(s) but {} index \
                                         expression(s) (report 8.1)",
                                        ix.len()
                                    ),
                                );
                            }
                            for e in ix {
                                let v = self.expr(e);
                                self.expect_integer(e.span, v.ty, "an array index");
                            }
                            var_record_param = false;
                            // `A[E1, E2]` means `A[E1][E2]` (report §8.1), so
                            // descend one level per index expression.
                            let mut stepped = elem;
                            for _ in 1..ix.len() {
                                stepped = self.types.elem(stepped).unwrap_or_else(|| self.types.error());
                            }
                            cur = Sym::Var {
                                ty: stepped,
                                read_only,
                            };
                            name = format!("{name}[…]");
                        }
                        None => {
                            let ty = self.error_ty(
                                d.span,
                                format!("`{name}` is not an array (report 8.1)"),
                            );
                            return Typed::error(ty);
                        }
                    }
                }
                Selector::Field(f) => {
                    let t = self.sym_type(&cur);
                    // report §8.1: `.` on a pointer implies dereferencing.
                    let rec = if self.types.is_pointer(t) {
                        self.types.deref(t).expect("checked")
                    } else {
                        t
                    };
                    match self.types.field(rec, f) {
                        Some(ft) => {
                            // ✔ rows 4 and 14 (the private-field halves). A field
                            // declared without the report's `*` is private to its
                            // module, so naming it through a type that came from
                            // an interface is an error — unless the type came
                            // from this module, which is the same check because
                            // only imported types are recorded (§6.3).
                            let decl = self.types.field_decl(rec, f).map(|(d, _)| d);
                            if let Some(idx) = decl
                                .and_then(|d| self.imported_types.get(&d).copied())
                            {
                                let declaring = self.types.get(decl.expect("checked above")).clone();
                                if let Type::Record { fields, .. } = declaring {
                                    if fields.iter().any(|field| {
                                        field.name == *f && !field.export
                                    }) {
                                        let module = self.interfaces[idx].module.clone();
                                        self.error(
                                            d.span,
                                            format!(
                                                "field `{f}` is private to module `{module}` (report 6.3)"
                                            ),
                                        );
                                    }
                                }
                            }
                            var_record_param = var_record_param && self.types.is_record(ft);
                            cur = Sym::Var {
                                ty: ft,
                                read_only,
                            };
                            name = format!("{name}.{f}");
                        }
                        None => {
                            // ✔ rows 13 and 14.
                            let ty = if self.types.is_record(rec) {
                                self.error_ty(
                                    d.span,
                                    format!("`{}` has no field `{f}` (report 6.3)", self.describe(rec)),
                                )
                            } else {
                                self.error_ty(
                                    d.span,
                                    format!("`{name}` is not a record, so it has no field `{f}` (report 8.1)"),
                                )
                            };
                            return Typed::error(ty);
                        }
                    }
                }
                Selector::Deref => {
                    let t = self.sym_type(&cur);
                    match self.types.deref(t) {
                        Some(b) => {
                            var_record_param = false;
                            cur = Sym::Var {
                                ty: b,
                                read_only,
                            };
                            name = format!("{name}^");
                        }
                        None => {
                            // ✔ row 12.
                            let ty = self.error_ty(
                                d.span,
                                format!("`{name}` is not a pointer (report 8.1)"),
                            );
                            return Typed::error(ty);
                        }
                    }
                }
                Selector::Guard(q) => {
                    let t = self.sym_type(&cur);
                    let tested = match self.lookup_qual(q, d.span) {
                        Some(Sym::Type(x)) => x,
                        Some(_) => {
                            let e = self.error_ty(d.span, format!("`{}` is not a type", q.join(".")));
                            return Typed::error(e);
                        }
                        None => return Typed::error(self.types.error()),
                    };
                    let probe = Typed {
                        ty: t,
                        var_record_param,
                        ..Typed::value(t)
                    };
                    let narrowed = self
                        .guard_type(d.span, &probe, tested, "a type guard")
                        .unwrap_or(tested);
                    var_record_param = false;
                    cur = Sym::Var {
                        ty: narrowed,
                        read_only,
                    };
                    name = format!("{name}({})", q.join("."));
                }
                Selector::Args(_) => {
                    // The parser produces this and `reclassify` rewrites it; a
                    // survivor means the caller skipped reclassification.
                    self.error(
                        d.span,
                        "internal error: an unresolved selector survived reclassification",
                    );
                    return Typed::error(self.types.error());
                }
            }
        }

        let ty = self.sym_type(&cur);
        let mut t = match &cur {
            Sym::Var { read_only: ro, .. } => Typed {
                lvalue: true,
                // An imported variable is read-only for assignment but may be
                // handed to a VAR parameter (report §11, and the report's own
                // `Texts.Append(Oberon.Log, W.buf)`).
                var_ok: true,
                read_only: *ro,
                ..Typed::value(ty)
            },
            Sym::Param { ty, by_var } => {
                // report §10.1: a value parameter of a basic type "represents a
                // local variable"; a structured one is read-only. A VAR
                // parameter is a variable, and a VAR *record* parameter is what
                // makes a guard, an `IS` test or a type CASE applicable to it
                // (report §8.1, §9.5).
                let is_rec = self.types.is_record(*ty);
                if *by_var || !self.is_structured(*ty) {
                    Typed {
                        lvalue: true,
                        var_ok: true,
                        var_record_param: *by_var && is_rec,
                        ..Typed::value(*ty)
                    }
                } else {
                    Typed {
                        lvalue: false,
                        var_record_param: false,
                        read_only: Some("is a structured value parameter, which is read-only (report 10.1)"),
                        ..Typed::value(*ty)
                    }
                }
            }
            // ✔ row 6: report §9.1, a constant is not assignable.
            Sym::Const(..) => Typed {
                lvalue: false,
                read_only: Some("is a constant, which is read-only (report 9.1)"),
                ..Typed::value(ty)
            },
            Sym::Proc { local, .. } => {
                Typed {
                    proc: Some(if *local {
                        "local procedure"
                    } else {
                        "procedure"
                    }),
                    ..Typed::value(ty)
                }
            }
            Sym::Predef(n) => {
                // ✔ row 51: report §6.5/§10.1 - a standard procedure cannot be
                // used as a value.
                let ty = self.error_ty(
                    d.span,
                    format!("`{n}` is a predeclared procedure: it cannot be used as a value (report 10.1)"),
                );
                Typed::error(ty)
            }
            Sym::Type(_) => {
                let ty = self.error_ty(d.span, format!("`{name}` is a type, not a value (report 4)"));
                Typed::error(ty)
            }
            Sym::Module(_) => {
                let ty = self.error_ty(
                    d.span,
                    format!("`{name}` is a module, not a value (report 11)"),
                );
                Typed::error(ty)
            }
        };
        if var_record_param && self.types.is_record(t.ty) {
            t.var_record_param = true;
        }
        t.name = Some(name);
        t
    }

    fn sym_type(&self, s: &Sym) -> TypeId {
        match s {
            Sym::Const(_, t) => *t,
            Sym::Type(t) => *t,
            Sym::Var { ty, .. } => *ty,
            Sym::Param { ty, .. } => *ty,
            Sym::Proc { ty, .. } => *ty,
            Sym::Predef(_) | Sym::Module(_) => self.types.error(),
        }
    }

    fn array_dims(&self, t: TypeId) -> usize {
        let mut n = 0;
        let mut cur = t;
        while let Some(e) = self.types.elem(cur) {
            n += 1;
            cur = e;
        }
        n
    }

    fn signature(&self, t: TypeId) -> (Vec<Param>, Option<TypeId>) {
        match self.types.get(t) {
            Type::Proc { params, result } => (params.clone(), *result),
            _ => (Vec::new(), None),
        }
    }

    /// ✔ rows 25, 26, 27.
    fn check_actuals(&mut self, span: Span, formals: &[Param], args: &[Expr]) {
        if formals.len() != args.len() {
            self.error(
                span,
                format!(
                    "expected {} actual parameter(s), found {} (report 9.2)",
                    formals.len(),
                    args.len()
                ),
            );
            for a in args {
                self.expr(a);
            }
            return;
        }
        for (f, a) in formals.iter().zip(args) {
            let v = self.expr(a);
            // ✔ row 50, the locality half: report §6.5/§10.1 — a formal of
            // procedure type takes "a procedure declared globally, or a variable
            // (or parameter) of that procedure type", never one declared inside
            // another procedure. The formal's type is checked first so that a
            // plain type mismatch is not reported twice.
            if let Some("local procedure") = v.proc {
                if matches!(self.types.get(f.ty), Type::Proc { .. }) {
                    self.error(
                        a.span,
                        "a procedure declared inside another procedure cannot be passed as a \
                         procedure parameter (report 6.5, 10.1)",
                    );
                }
            }
            if f.by_var && !v.lvalue {
                self.error(
                    a.span,
                    "a VAR parameter needs an actual that is a variable (report 9.2)",
                );
            }
            if f.by_var && !v.var_ok {
                if let Some(reason) = v.read_only {
                    self.error(a.span, format!("a VAR parameter needs a variable: it {reason}"));
                }
            }
            if !self.types.param_ok(f, v.ty) {
                self.error(
                    a.span,
                    format!(
                        "the actual parameter's type does not match the formal's {} (report 10.1)",
                        self.describe(f.ty)
                    ),
                );
            }
        }
    }

    /// Whether a designator names a *function* procedure, asked without
    /// reporting anything (see the call-statement check).
    fn callee_is_function(&self, q: &QIdent) -> Option<bool> {
        if q.len() == 1 {
            return match self.lookup(&q[0])? {
                Sym::Proc { ty, .. } => Some(self.signature(ty).1.is_some()),
                Sym::Var { ty, .. } if self.types.is_proc(ty) => {
                    Some(self.signature(ty).1.is_some())
                }
                Sym::Predef(n) => Some(PREDEFINED_FUNCTIONS.contains(&n)),
                _ => None,
            };
        }
        if q.len() != 2 {
            return None;
        }
        let idx = match self.lookup(&q[0]) {
            Some(Sym::Module(i)) => i,
            _ => return None,
        };
        self.interfaces[idx]
            .items
            .iter()
            .find(|i| item_name(i) == q[1])
            .and_then(|i| match i {
                Item::Proc { result, .. } => Some(result.is_some()),
                _ => None,
            })
    }

    /// A mutating predefined procedure must be handed a writable variable: the
    /// same rules as an assignment target apply (report §9.1, §9.8, §10.1).
    fn note_mutation(&mut self, a: &Expr) {
        let v = self.expr(a);
        let what = match &a.kind {
            ExprKind::Designator(d) => d.base.join("."),
            _ => "this expression".to_string(),
        };
        if !matches!(a.kind, ExprKind::Designator(_)) {
            self.error(a.span, format!("`{what}` must be a variable, not an expression (report 10.2)"));
            return;
        }
        if let Some(reason) = v.read_only {
            self.error(a.span, format!("`{what}` {reason}"));
        }
        if let Some(name) = &v.name {
            if self.locked.iter().any(|n| n == name) {
                self.error(
                    a.span,
                    format!("`{name}` is the control variable of an enclosing FOR loop (report 9.8)"),
                );
            }
        }
    }

    /// A predeclared procedure of report §10.2, used as a call.
    fn predef_call(&mut self, span: Span, name: &str, args: &[Expr]) -> Typed {
        let mut tys = Vec::new();
        let mut typed: Vec<Typed> = Vec::new();
        for a in args {
            let v = self.expr(a);
            tys.push(v.ty);
            typed.push(v);
        }
        // report §9.1 and §9.8: whatever a *mutating* predefined procedure
        // writes must be a writable variable - not a constant, not a read-only
        // (imported or structured value) object, and not a FOR control variable.
        // A function's arguments are values and need no such check.
        if matches!(name, "INC" | "DEC" | "INCL" | "EXCL" | "NEW" | "PACK" | "UNPK") {
            if let Some(first) = args.first() {
                self.note_mutation(first);
            }
        }
        if PREDEFINED_FUNCTIONS.contains(&name) {
            return match self.types.predefined(name, &tys) {
                Ok(t) => Typed::value(t),
                Err(msg) => Typed::error(self.error_ty(span, msg)),
            };
        }
        // The proper procedures (report §10.2).
        match name {
            "INC" | "DEC" => {
                if !matches!(args.len(), 1 | 2) {
                    self.error(span, format!("{name} takes 1 or 2 arguments (report 10.2)"));
                }
                if let Some(t) = tys.first() {
                    self.expect_integer(args[0].span, *t, &format!("argument 1 of {name}"));
                }
                if let Some(b) = args.get(1) {
                    self.expect_integer(b.span, tys[1], &format!("argument 2 of {name}"));
                }
            }
            "INCL" | "EXCL" => {
                if tys.len() != 2 {
                    self.error(span, format!("{name} takes 2 arguments (report 10.2)"));
                } else {
                    if !self.types.is_set(tys[0]) {
                        self.error(args[0].span, format!("argument 1 of {name} must be a SET (report 10.2)"));
                    }
                    self.expect_integer(args[1].span, tys[1], &format!("argument 2 of {name}"));
                }
            }
            "NEW" => {
                if tys.len() != 1 {
                    self.error(span, "NEW takes 1 argument (report 10.2)");
                } else if !self.types.is_pointer(tys[0]) {
                    self.error(args[0].span, "the argument of NEW must be a pointer variable (report 10.2)");
                }
            }
            "PACK" => {
                if tys.len() != 2 {
                    self.error(span, "PACK takes 2 arguments (report 10.2)");
                } else {
                    // `PACK(x, n)` scales x by 2^n: x is written, n is not
                    // (report §10.2), so only x has to be a variable here.
                    if !matches!(self.types.get(tys[0]), Type::Basic(Basic::Real)) {
                        self.error(args[0].span, "argument 1 of PACK must be a REAL variable (report 10.2)");
                    }
                    if !matches!(args[1].kind, ExprKind::Designator(_)) && tys[1] == self.types.error() {
                        self.error(args[1].span, "argument 2 of PACK must be an INTEGER (report 10.2)");
                    }
                    self.expect_integer(args[1].span, tys[1], "argument 2 of PACK");
                }
            }
            "UNPK" => {
                if tys.len() != 2 {
                    self.error(span, "UNPK takes 2 arguments (report 10.2)");
                } else {
                    if !matches!(self.types.get(tys[0]), Type::Basic(Basic::Real)) {
                        self.error(args[0].span, "argument 1 of UNPK must be a REAL variable (report 10.2)");
                    }
                    self.expect_integer(args[1].span, tys[1], "argument 2 of UNPK");
                    self.note_mutation(&args[1]);
                }
            }
            "ASSERT" => {
                if tys.len() != 1 {
                    self.error(span, "ASSERT takes 1 argument (report 10.2)");
                } else if !self.types.is_bool(tys[0]) {
                    self.error(args[0].span, "the argument of ASSERT must be a BOOLEAN (report 10.2)");
                }
            }
            _ => self.error(span, format!("`{name}` is not a predeclared procedure")),
        }
        Typed::value(self.types.error())
    }

    // ----------------------------------------------------------- statements

    fn stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Assign { target, value } => {
                let t = self.designator(target);
                let v = self.expr(value);
                if let Some(reason) = t.read_only {
                    // ✔ row 6.
                    self.error(s.span, format!("cannot assign to this: it {reason}"));
                }
                if let Some(name) = &t.name {
                    // ✔ row 32: the FOR control variable is frozen in the body.
                    if self.locked.iter().any(|n| n == name) {
                        self.error(
                            s.span,
                            format!("`{name}` is the control variable of an enclosing FOR loop (report 9.8)"),
                        );
                    }
                }
                if !t.lvalue && !self.types.is_error(t.ty) {
                    self.error(
                        s.span,
                        "the left-hand side of an assignment must be a variable (report 9.1)",
                    );
                }
                // ✔ rows 7, 8, 9, 10: one predicate for all four exceptions.
                if !self.types.is_assignable(t.ty, v.ty) {
                    let note = if self.types.is_numeric(t.ty) && self.types.is_numeric(v.ty) {
                        // The one conversion Oberon refuses to do silently.
                        "there is no implicit conversion between INTEGER and REAL; use FLT or FLOOR (report 8.2.2)"
                    } else if self.types.is_nil(v.ty) {
                        "NIL may only be assigned to a pointer or a procedure variable (report 9.1)"
                    } else if self.types.is_record(t.ty) {
                        "a record may only be assigned from an extension of its own type (report 9.1)"
                    } else {
                        ""
                    };
                    let msg = format!(
                        "cannot assign {} to {} (report 9.1)",
                        self.describe(v.ty),
                        self.describe(t.ty)
                    );
                    if note.is_empty() {
                        self.error(s.span, msg);
                    } else {
                        self.error_note(s.span, msg, note);
                    }
                }
                // ✔ rows 50, 51: report §6.5 - a local or predeclared procedure
                // cannot be the value assigned to a procedure variable.
                if let Some(kind) = v.proc {
                    if kind == "local procedure" {
                        self.error(
                            s.span,
                            "a procedure declared inside another procedure cannot be assigned to \
                             a procedure variable (report 6.5)",
                        );
                    }
                }
            }
            StmtKind::Call(d) => {
                let ends_in_call = matches!(d.sels.last(), Some(Selector::Call(_)));
                if !ends_in_call {
                    // A bare designator: the call of a parameterless procedure.
                    match self.lookup_qual(&d.base, d.span) {
                        Some(Sym::Proc { ty, .. }) => {
                            let (params, _) = self.signature(ty);
                            if !params.is_empty() {
                                // ✔ row 30 (and row 49).
                                self.error(
                                    d.span,
                                    format!(
                                        "`{}` has {} parameter(s): an actual parameter list is required (report 9.2)",
                                        d.base.join("."),
                                        params.len()
                                    ),
                                );
                            }
                        }
                        Some(Sym::Var { ty, .. }) if self.types.is_proc(ty) => {
                            let (params, _) = self.signature(ty);
                            if !params.is_empty() {
                                self.error(
                                    d.span,
                                    "a procedure variable with parameters must be called with an \
                                     argument list (report 9.2)",
                                );
                            }
                        }
                        // `WriteLn`-style calls of a predeclared function used
                        // as a statement are caught below by the function check.
                        Some(Sym::Predef(n)) => {
                            if PREDEFINED_FUNCTIONS.contains(&n) {
                                // ✔ row 31: a function's value must be used.
                                self.error(
                                    d.span,
                                    format!("`{n}` is a function procedure: its value must be used (report 9)"),
                                );
                            }
                        }
                        Some(_) => {
                            let name = d.base.join(".");
                            self.error(d.span, format!("`{name}` is not a procedure (report 9.2)"));
                        }
                        None => {}
                    }
                    return;
                }
                // ✔ row 31: calling a function for its value in statement
                // position leaves the value unused. The question is answered by
                // a *silent* lookup: `designator` is the one place that reports
                // a bad callee, so it must not report it twice.
                let is_function = self.callee_is_function(&d.base).unwrap_or(false);
                let t = self.designator(d);
                let _ = t;
                if is_function {
                    self.error(
                        s.span,
                        "a function procedure must be used for its value, not as a statement \
                         (report 9)",
                    );
                }
            }
            StmtKind::If { arms, els } => {
                for (c, b) in arms {
                    let v = self.expr(c);
                    // ✔ row 24.
                    if !self.types.is_bool(v.ty) && !self.types.is_error(v.ty) {
                        self.error(c.span, "the guard of an IF must be a BOOLEAN (report 9.4)");
                    }
                    for s in b {
                        self.stmt(s);
                    }
                }
                if let Some(b) = els {
                    for s in b {
                        self.stmt(s);
                    }
                }
            }
            StmtKind::While { arms } => {
                for (c, b) in arms {
                    let v = self.expr(c);
                    if !self.types.is_bool(v.ty) && !self.types.is_error(v.ty) {
                        self.error(c.span, "the guard of a WHILE must be a BOOLEAN (report 9.6)");
                    }
                    for s in b {
                        self.stmt(s);
                    }
                }
            }
            StmtKind::Repeat { body, until } => {
                for s in body {
                    self.stmt(s);
                }
                let v = self.expr(until);
                if !self.types.is_bool(v.ty) && !self.types.is_error(v.ty) {
                    self.error(until.span, "the UNTIL condition must be a BOOLEAN (report 9.7)");
                }
            }
            StmtKind::For {
                var,
                from,
                to,
                by,
                body,
            } => {
                let f = self.expr(from);
                let t = self.expr(to);
                self.expect_integer(from.span, f.ty, "the initial value of a FOR");
                self.expect_integer(to.span, t.ty, "the limit of a FOR");
                // ✔ row 32: report §9.8 - the control variable is an INTEGER
                // variable of the *current* scope.
                match self.lookup(var) {
                    Some(Sym::Var { ty, read_only: None }) if self.types.is_integer_like(ty) => {}
                    Some(Sym::Var { ty, .. }) if self.types.is_integer_like(ty) => self.error(
                        s.span,
                        format!("the control variable `{var}` of a FOR may not be read-only (report 9.8)"),
                    ),
                    Some(Sym::Var { .. }) => self.error(
                        s.span,
                        format!("`{var}` is not an INTEGER variable (report 9.8)"),
                    ),
                    Some(_) => self.error(
                        s.span,
                        format!("`{var}` is not a variable (report 9.8)"),
                    ),
                    None => self.error(
                        s.span,
                        format!("`{var}` is not declared, so it cannot be the control variable of a FOR (report 9.8)"),
                    ),
                }
                // ✔ row 34.
                if let Some(b) = by {
                    match self.const_expr(b) {
                        Ok(Const::Int(0)) => self.error(
                            b.span,
                            "the BY increment of a FOR must not be zero (report 9.8)",
                        ),
                        Ok(Const::Int(_)) => {}
                        Ok(_) => self.error(
                            b.span,
                            "the BY increment of a FOR must be an integer constant (report 9.8)",
                        ),
                        Err(msg) => self.error(b.span, msg),
                    }
                }
                self.locked.push(var.clone());
                for s in body {
                    self.stmt(s);
                }
                self.locked.pop();
            }
            StmtKind::Case { subject, arms } => self.case_stmt(s, subject, arms),
        }
    }

    /// ✔ rows 35, 36, 37, 38, 39: report §9.5.
    /// Robustly record a CASE label interval, reporting an empty range or an
    /// overlap with an earlier label (report §9.5: the labels must be distinct).
    fn note_label(&mut self, span: Span, lo: i32, hi: i32, seen: &mut Vec<(i32, i32)>) {
        if hi < lo {
            self.error(span, format!("the CASE range {lo}..{hi} has no elements (report 9.5)"));
            return;
        }
        if let Some((a, b)) = seen.iter().find(|(a, b)| !(hi < *a || lo > *b)) {
            self.error(
                span,
                format!("CASE label {lo}..{hi} overlaps the earlier range {a}..{b} (report 9.5)"),
            );
            return;
        }
        seen.push((lo, hi));
    }

    fn case_stmt(&mut self, s: &Stmt, subject: &Expr, arms: &[ast::CaseArm]) {
        let sub = self.expr(subject);
        let is_type_case = self.types.is_pointer(sub.ty) || self.types.is_record(sub.ty);
        if is_type_case && !self.types.is_pointer(sub.ty) && !sub.var_record_param {
            // ✔ row 38.
            self.error(
                subject.span,
                "a type CASE needs a pointer or a VAR record parameter (report 9.5)",
            );
        }
        // A label must extend the case variable's type; for a pointer variable
        // the report's own §9.5 example writes the label as a pointer type, and
        // the §8.1 spelling (a record extending the pointer's base) is accepted
        // too - see `guard_type`.
        let pointer_base = if self.types.is_pointer(sub.ty) {
            self.types.deref(sub.ty)
        } else {
            None
        };
        // Ordinal labels are checked as *intervals*: a range covers its
        // interior, so `1..3` and `2` overlap even though their endpoints
        // differ, and a descending range has no elements at all.
        let mut seen_ord: Vec<(i32, i32)> = Vec::new();
        let mut seen_type: Vec<TypeId> = Vec::new();
        let mut has_default = false;
        // report §9.5: "in the statements S_i labelled by T_i, the case variable
        // is considered as of type T_i". The name is rebound per arm, exactly as
        // the emitter rebinds it.
        let case_name = match &subject.kind {
            ExprKind::Designator(d) if d.base.len() == 1 && d.sels.is_empty() => Some(d.base[0].clone()),
            _ => None,
        };
        let case_sym = case_name.as_ref().and_then(|n| self.lookup(n));
        for arm in arms {
            // The arm's view of the case variable (see above).
            let arm_view: Option<TypeId> = arm.labels.as_ref().and_then(|labels| {
                labels.iter().find_map(|l| match &l.lo {
                    // Silent on purpose: `lookup_qual` reports an undeclared
                    // name, and the label check below is the place that does.
                    CaseLabel::Named(q) => self.type_of_qual_silent(q),
                    _ => None,
                })
            });
            self.push_scope();
            if let (Some(name), Some(sym), Some(ty)) = (&case_name, &case_sym, arm_view) {
                let read_only = match sym {
                    Sym::Var { read_only, .. } => *read_only,
                    _ => None,
                };
                self.declare(
                    name,
                    Sym::Var {
                        ty,
                        read_only,
                    },
                    arm.span,
                );
            }
            match &arm.labels {
                None => has_default = true,
                Some(labels) => {
                    for l in labels {
                        // One label per entry: a range is `l.lo .. l.hi`, and the
                        // upper end is *part of the interval*, not a second label.
                        for lab in std::iter::once(&l.lo) {
                            match lab {
                                ast::CaseLabel::Int(v) => {
                                    if self.types.is_error(sub.ty) {
                                        continue;
                                    }
                                    if is_type_case {
                                        self.error(
                                            arm.span,
                                            "this CASE selects over a record or pointer, so its labels must be type names (report 9.5)",
                                        );
                                        continue;
                                    }
                                    if !self.types.is_integer_like(sub.ty) {
                                        // ✔ row 35.
                                        self.error(
                                            arm.span,
                                            "an integer label needs an INTEGER case variable (report 9.5)",
                                        );
                                    }
                                    // ✔ row 36, including the range interior.
                                    let hi = l.hi.as_ref().map(|h| self.case_label_ord(h));
                                    self.note_label(arm.span, *v as i32, hi.unwrap_or(*v as i32), &mut seen_ord);
                                }
                                ast::CaseLabel::Char(c) => {
                                    if self.types.is_error(sub.ty) {
                                        continue;
                                    }
                                    if !self.types.is_char(sub.ty) {
                                        self.error(
                                            arm.span,
                                            "a character label needs a CHAR case variable (report 9.5)",
                                        );
                                    }
                                    self.note_label(arm.span, *c as i32, *c as i32, &mut seen_ord);
                                }
                                ast::CaseLabel::Str(text) => {
                                    if self.types.is_error(sub.ty) {
                                        continue;
                                    }
                                    if text.len() == 1 && self.types.is_char(sub.ty) {
                                        let n = text.as_bytes()[0] as i32;
                                        self.note_label(arm.span, n, n, &mut seen_ord);
                                    } else {
                                        // ✔ row 35.
                                        self.error(
                                            arm.span,
                                            "a string label must be a single character for a CHAR case variable (report 9.5)",
                                        );
                                    }
                                }
                                ast::CaseLabel::Named(q) => {
                                    if l.hi.is_some() {
                                        // The emitter dispatches on a type id, so
                                        // a range of types has no meaning here.
                                        self.error(
                                            arm.span,
                                            "a type CASE label is a single type name, not a range (report 9.5)",
                                        );
                                        continue;
                                    }
                                    if !is_type_case {
                                        // ✔ row 35.
                                        self.error(
                                            arm.span,
                                            format!(
                                                "`{}` cannot be a label here: the case variable is not a record or pointer (report 9.5)",
                                                q.join(".")
                                            ),
                                        );
                                        continue;
                                    }
                                    match self.lookup_qual(q, arm.span) {
                                        Some(Sym::Type(t)) => {
                                            // ✔ row 37.
                                            let ok = self.types.extends(t, sub.ty)
                                                || pointer_base
                                                    .map(|b| self.types.extends(t, b))
                                                    .unwrap_or(false);
                                            if !ok {
                                                self.error(
                                                    arm.span,
                                                    format!(
                                                        "`{}` is not an extension of the case variable's type (report 9.5)",
                                                        q.join(".")
                                                    ),
                                                );
                                            }
                                            if seen_type.contains(&t) {
                                                self.error(arm.span, "duplicate type label in CASE (report 9.5)");
                                            }
                                            seen_type.push(t);
                                        }
                                        Some(_) => self.error(
                                            arm.span,
                                            format!("`{}` is not a type (report 9.5)", q.join(".")),
                                        ),
                                        None => {}
                                    }
                                }
                            }
                        }
                    }
                }
            }
            for s in &arm.body {
                self.stmt(s);
            }
            // The arm's scope holds the narrowed case variable and *only* the
            // arm: leaving it open would leak the narrowing into everything
            // that follows.
            self.pop_scope();
        }
        // ✔ row 39: without a default, a value matching no label falls through.
        if is_type_case && !has_default {
            self.warn(
                s.span,
                "this type CASE has no default arm: a value matching no label would be undefined \
                 (report 9.5)",
            );
        }
    }

    /// The ordinal of a `CaseLabel` used as a range end.
    fn case_label_ord(&self, l: &CaseLabel) -> i32 {
        match l {
            CaseLabel::Int(v) => *v as i32,
            CaseLabel::Char(c) => *c as i32,
            CaseLabel::Str(s) => s.as_bytes().first().copied().unwrap_or(0) as i32,
            CaseLabel::Named(_) => 0,
        }
    }

    fn describe(&self, t: TypeId) -> String {
        match self.types.get(t) {
            Type::Basic(Basic::Integer) | Type::Basic(Basic::Byte) => "an INTEGER".into(),
            Type::Basic(Basic::Real) => "a REAL".into(),
            Type::Basic(Basic::Char) => "a CHAR".into(),
            Type::Basic(Basic::Boolean) => "a BOOLEAN".into(),
            Type::Basic(Basic::Set) => "a SET".into(),
            Type::Nil => "NIL".into(),
            Type::StringLit(n) => format!("a {n}-character string"),
            Type::Array { len, .. } => format!("an ARRAY {len} OF …"),
            Type::OpenArray { .. } => "an open ARRAY OF …".into(),
            Type::Record { .. } => "a record".into(),
            Type::Pointer { .. } => "a pointer".into(),
            Type::Proc { .. } => "a procedure".into(),
            Type::Error => "an error".into(),
        }
    }
}

/// The type names of the section being resolved: `placeholders` are the
/// record-bodied declarations whose identity is known before their fields
/// (report §6.4's forward reference), `names` is every name the section
/// declares, so a reference to a *later* non-record can be told apart from a
/// reference to something that does not exist at all.
#[derive(Default)]
struct TypeScope {
    placeholders: HashMap<String, TypeId>,
    names: std::collections::HashSet<String>,
    /// Names whose declaration has already been built in this section.
    defined: std::collections::HashSet<String>,
    /// The built types, which is what the caller gets back.
    result: HashMap<String, TypeId>,
}

/// Report §10.2, the function procedures and type conversions.
const PREDEFINED_FUNCTIONS: &[&str] = &[
    "ABS", "ODD", "LEN", "LSL", "ASR", "ROR", "FLOOR", "FLT", "ORD", "CHR",
];
/// Report §10.2, the proper procedures.
const PREDEFINED_PROPER: &[&str] = &[
    "INC", "DEC", "INCL", "EXCL", "NEW", "ASSERT", "PACK", "UNPK",
];

/// Parse a type as an interface file renders it, using the module grammar.
fn rendered_type(text: &str) -> Option<TypeExpr> {
    let mut lexer = crate::lexer::Lexer::new(text);
    let mut errors = Vec::new();
    match crate::oberon::RenderedTypeParser::new().parse(&mut errors, &mut lexer) {
        // An interface rendering is either readable or it is not; there is no
        // recovery inside one type.
        Ok(t) if errors.is_empty() => Some(t),
        _ => None,
    }
}

/// The span a type is filed under. Spans are unique per node in practice; two
/// nodes can share one (a designator and the expression wrapping it, an
/// expression and its parentheses), and those always have the same type.
pub fn span_key(span: Span) -> u64 {
    key_of(span)
}

fn key_of(span: Span) -> u64 {
    ((span.lo as u64) << 32) | span.hi as u64
}

fn item_name(i: &Item) -> &str {
    match i {
        Item::Const { name, .. }
        | Item::Type { name, .. }
        | Item::Var { name, .. }
        | Item::Proc { name, .. } => name,
    }
}

/// A constant as an interface file renders it: the forms our own writer emits.
fn parse_const_text(text: &str) -> Option<Const> {
    let t = text.trim();
    if let Some(s) = t.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        return Some(Const::Str(s.to_string()));
    }
    if t == "TRUE" {
        return Some(Const::Bool(true));
    }
    if t == "FALSE" {
        return Some(Const::Bool(false));
    }
    if let Some(hex) = t.strip_suffix('H') {
        let (neg, digits) = match hex.strip_prefix('-') {
            Some(d) => (true, d),
            None => (false, hex),
        };
        let v = u32::from_str_radix(digits, 16).ok()?;
        let v = v as i32;
        return Some(Const::Int(if neg { v.wrapping_neg() } else { v }));
    }
    if let Some(hex) = t.strip_suffix('X') {
        // A character constant, as `oberon` writes one (report §3).
        let v = u32::from_str_radix(hex, 16).ok()?;
        return Some(Const::Char((v & 0xFF) as u8));
    }
    if let Some(inner) = t.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
        // A set as a list of ranges, which is how the writer renders it.
        let mut bits = 0u32;
        for part in inner.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let (lo, hi) = match part.split_once("..") {
                Some((a, b)) => (a.trim().parse::<i32>().ok()?, b.trim().parse::<i32>().ok()?),
                None => {
                    let v = part.parse::<i32>().ok()?;
                    (v, v)
                }
            };
            bits |= const_eval::range(lo, hi).ok()?;
        }
        return Some(Const::Set(bits));
    }
    t.parse::<i32>().ok().map(Const::Int)
}

/// The literal node for a folded constant.
fn literal(v: &Const) -> ExprKind {
    match v {
        Const::Int(n) => ExprKind::Int(*n as u32),
        Const::Real(r) => ExprKind::Real(*r as f64),
        Const::Bool(b) => ExprKind::Bool(*b),
        Const::Char(c) => ExprKind::Char(*c),
        Const::Str(s) => ExprKind::Str(s.clone()),
        Const::Set(bits) => {
            // A set constant becomes a range-free element list, which
            // `const_eval` folds back to the same bit pattern.
            let mut elems = Vec::new();
            for i in 0..32u32 {
                if bits & (1 << i) != 0 {
                    elems.push(ast::SetElem {
                        lo: Expr {
                            kind: ExprKind::Int(i),
                            span: Span::default(),
                        },
                        hi: None,
                    });
                }
            }
            ExprKind::Set(elems)
        }
    }
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
