//! obgen — a random Oberon-07 program generator, used as a self-checking oracle.
//!
//! This is the classical way to test a compiler: generate programs that are
//! *known* to be well formed and *known* to compute a particular value, so any
//! disagreement is a bug rather than a judgement call.
//!
//! How this file gets both:
//!
//! * It generates a small structured IR (`Expr` / `Stmt`), renders that as
//!   Oberon-07, and *interprets the same IR* with its own arithmetic. Every
//!   module variable's expected value therefore comes from this file, not from
//!   the compiler, so the oracle is not circular.
//! * The emitted program ends with one check per module variable:
//!
//!   ```oberon
//!   IF m0 # 42 THEN fail[1] := 0 END;  (* check m0 expected 42 *)
//!   ```
//!
//!   `fail` is an `ARRAY 1 OF INTEGER`, so the failing branch is an
//!   out-of-range index: it traps with the source offset of the designator
//!   (DESIGN §9.4), and the runner maps that offset back to this line. The
//!   generated program needs no output at all, and the checks cannot be
//!   optimised away because they have an observable effect.
//!
//! The fragment is chosen so that a failure is never "undefined behaviour":
//! `INTEGER` variables, constants, `+ - * DIV MOD`, `ABS`, `MIN`/`MAX`,
//! the four shifts, relations as conditions, assignment, `IF`, `WHILE`,
//! `REPEAT`, `FOR`, `CASE`, calls to generated procedures with value
//! parameters, and one recursive function (the report allows a single `RETURN`,
//! at the end of the body, so the accumulator shape it uses is the idiomatic
//! escaped form of an early return).
//!
//! Excluded on purpose, because the report does not define them and a
//! disagreement would not be a bug: `DIV`/`MOD` by zero, `MIN(INTEGER) DIV -1`,
//! and shifts by a negative count. Overflow *is* included: the design fixes
//! `-fwrapv` (DESIGN §9.1, R4), so wrapping is the specified behaviour.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use oberon_front::line_col;

// ---------------------------------------------------------------- random ---

/// xorshift64*, so a seed reproduces a program exactly, on any machine.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + self.below((hi - lo + 1) as usize) as i32
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

// ----------------------------------------------------------- expressions ---

#[derive(Clone, Copy, Debug)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

impl Op {
    fn name(self) -> &'static str {
        match self {
            Op::Add => "+",
            Op::Sub => "-",
            Op::Mul => "*",
            Op::Div => "DIV",
            Op::Mod => "MOD",
        }
    }

    fn apply(self, l: i32, r: i32) -> i32 {
        match self {
            Op::Add => l.wrapping_add(r),
            Op::Sub => l.wrapping_sub(r),
            Op::Mul => l.wrapping_mul(r),
            Op::Div => div_floor(l, r),
            Op::Mod => l.wrapping_sub(div_floor(l, r).wrapping_mul(r)),
        }
    }
}

/// The report's `DIV` is floor division. The generator only ever divides by a
/// positive constant, which keeps `MIN DIV -1` (whose mathematical result is not
/// representable) out of the programs entirely.
fn div_floor(l: i32, r: i32) -> i32 {
    let q = l.wrapping_div(r);
    if l.wrapping_rem(r) != 0 && ((l < 0) != (r < 0)) {
        q - 1
    } else {
        q
    }
}

/// The shifts of report §10.2 (there is no `ROL`). `LSL` and `ASR` are defined
/// as multiplication and division by `2^n`, and `ROR` as a rotation. Counts of
/// 32 and above are generated on purpose: multiplying by `2^32` wraps to zero
/// and `x DIV 2^32` is 0 or -1, so the report's answer is defined even where
/// C's `<<` and `>>` are not.
fn shift(name: &str, x: i32, n: u32) -> i32 {
    match name {
        "LSL" => {
            if n >= 32 {
                0
            } else {
                ((x as i64) * (1i64 << n)) as i32
            }
        }
        "ASR" => {
            if n >= 32 {
                if x < 0 {
                    -1
                } else {
                    0
                }
            } else {
                ((x as i64) >> n) as i32
            }
        }
        "ROR" => x.rotate_right(n % 32),
        other => panic!("unknown shift {other}"),
    }
}

#[derive(Clone, Debug)]
enum Expr {
    /// An `INTEGER` literal. `MIN(INTEGER)` is spelled out: `-2147483648` is not
    /// a legal literal (see the lexical near misses in DESIGN §4).
    Int(i32),
    MinInt,
    MaxInt,
    /// A named constant of the generated `CONST` block.
    Konst(usize),
    /// A module variable (`m<k>`).
    MVar(usize),
    /// A parameter or local of the procedure being rendered (`loc[i]`).
    LVar(usize),
    Neg(Box<Expr>),
    Bin(Op, Box<Expr>, Box<Expr>),
    Abs(Box<Expr>),
    Shift(&'static str, Box<Expr>, u32),
    /// A call to an earlier function procedure.
    Call(usize, Vec<Expr>),
}

impl Expr {
    fn render(&self, g: &Gen, loc: &[String]) -> String {
        match self {
            Expr::Int(i32::MIN) | Expr::MinInt => MIN_EXPR.to_string(),
            Expr::Int(k) if *k < 0 => format!("({k})"),
            Expr::Int(k) => k.to_string(),
            Expr::MaxInt => MAX_EXPR.to_string(),
            Expr::Konst(i) => g.consts[*i].0.to_string(),
            Expr::MVar(i) => g.mvars[*i].clone(),
            Expr::LVar(i) => loc[*i].clone(),
            Expr::Neg(e) => format!("(-{})", e.render(g, loc)),
            Expr::Bin(op, l, r) => {
                format!("({} {} {})", l.render(g, loc), op.name(), r.render(g, loc))
            }
            Expr::Abs(e) => format!("ABS({})", e.render(g, loc)),
            Expr::Shift(name, e, n) => format!("{name}({}, {n})", e.render(g, loc)),
            Expr::Call(f, args) => {
                let args: Vec<String> = args.iter().map(|a| a.render(g, loc)).collect();
                format!("{}({})", g.funcs[*f].name, args.join(", "))
            }
        }
    }

    /// The mirror. `m` is the module frame, `l` this call's parameters and
    /// locals, and `depth` bounds recursion (the generator only emits recursion
    /// that terminates).
    fn eval(&self, g: &Gen, m: &mut [i32], l: &[i32], depth: u32) -> i32 {
        match self {
            Expr::Int(k) => *k,
            Expr::MinInt => i32::MIN,
            Expr::MaxInt => i32::MAX,
            Expr::Konst(i) => g.consts[*i].1,
            Expr::MVar(i) => m[*i],
            Expr::LVar(i) => l[*i],
            Expr::Neg(e) => e.eval(g, m, l, depth).wrapping_neg(),
            Expr::Bin(op, a, b) => op.apply(a.eval(g, m, l, depth), b.eval(g, m, l, depth)),
            Expr::Abs(e) => e.eval(g, m, l, depth).wrapping_abs(),
            Expr::Shift(name, e, n) => shift(name, e.eval(g, m, l, depth), *n),
            Expr::Call(f, args) => {
                let args: Vec<i32> = args.iter().map(|a| a.eval(g, m, l, depth)).collect();
                g.call(*f, &args, m, depth + 1)
            }
        }
    }
}

#[derive(Clone, Debug)]
enum Cond {
    Rel(&'static str, Expr, Expr),
    Not(Box<Cond>),
    And(Box<Cond>, Box<Cond>),
}

impl Cond {
    fn render(&self, g: &Gen, loc: &[String]) -> String {
        match self {
            Cond::Rel(op, a, b) => format!("{} {op} {}", a.render(g, loc), b.render(g, loc)),
            Cond::Not(c) => format!("~({})", c.render(g, loc)),
            Cond::And(a, b) => format!("({}) & ({})", a.render(g, loc), b.render(g, loc)),
        }
    }

    fn eval(&self, g: &Gen, m: &mut [i32], l: &[i32], depth: u32) -> bool {
        match self {
            Cond::Rel(op, a, b) => {
                let (x, y) = (a.eval(g, m, l, depth), b.eval(g, m, l, depth));
                match *op {
                    "=" => x == y,
                    "#" => x != y,
                    "<" => x < y,
                    "<=" => x <= y,
                    ">" => x > y,
                    _ => x >= y,
                }
            }
            Cond::Not(c) => !c.eval(g, m, l, depth),
            Cond::And(a, b) => a.eval(g, m, l, depth) && b.eval(g, m, l, depth),
        }
    }
}

#[derive(Clone, Debug)]
enum Stmt {
    SetM(usize, Expr),
    SetL(usize, Expr),
    Block(Vec<Stmt>),
    If(Cond, Vec<Stmt>, Vec<Stmt>),
    While(Cond, Vec<Stmt>),
    Repeat(Vec<Stmt>, Cond),
    /// `FOR v := lo TO hi DO body END`; `v` is read-only inside the body.
    For(usize, i32, i32, Vec<Stmt>),
    /// `CASE e OF label: body | … END`. Oberon-07 has no `ELSE`, and the report's
    /// *default* arm — an empty label list followed by a statement sequence —
    /// cannot be written in LALR(1) at all (PLAN.md W6.4). The generator sidesteps
    /// both: the subject is a constant and one label always matches it, so no
    /// default arm is needed and every generated program is defined.
    Case(Expr, Vec<(i32, Vec<Stmt>)>),
    Call(usize, Vec<Expr>),
}

impl Stmt {
    fn exec(&self, g: &Gen, m: &mut [i32], l: &mut [i32], depth: u32) {
        match self {
            Stmt::SetM(i, e) => {
                let v = e.eval(g, m, l, depth);
                m[*i] = v;
            }
            Stmt::SetL(i, e) => {
                let v = e.eval(g, m, l, depth);
                l[*i] = v;
            }
            Stmt::Block(body) => {
                for s in body {
                    s.exec(g, m, l, depth);
                }
            }
            Stmt::If(c, then, els) => {
                let taken = c.eval(g, m, l, depth);
                for s in if taken { then } else { els } {
                    s.exec(g, m, l, depth);
                }
            }
            Stmt::While(c, body) => {
                let mut guard = 0;
                while c.eval(g, m, l, depth) {
                    for s in body {
                        s.exec(g, m, l, depth);
                    }
                    guard += 1;
                    assert!(
                        guard < 10_000,
                        "generated WHILE {} did not terminate",
                        c.render(g, &[])
                    );
                }
            }
            Stmt::Repeat(body, c) => {
                let mut guard = 0;
                loop {
                    for s in body {
                        s.exec(g, m, l, depth);
                    }
                    guard += 1;
                    assert!(
                        guard < 10_000,
                        "generated REPEAT UNTIL {} did not terminate",
                        c.render(g, &[])
                    );
                    if c.eval(g, m, l, depth) {
                        break;
                    }
                }
            }
            Stmt::For(v, lo, hi, body) => {
                let mut i = *lo;
                while i <= *hi {
                    m[*v] = i;
                    for s in body {
                        s.exec(g, m, l, depth);
                    }
                    i += 1;
                }
                // Report §9.8 defines the loop as `v := beg; WHILE v <= end DO S;
                // v := v + inc END`, so the control variable ends at `end + inc` —
                // which is exactly what the emitted C's `for` header leaves. The
                // mirror used to stop at the last body value and disagree with
                // every program that read the variable afterwards.
                m[*v] = i;
            }
            Stmt::Case(e, arms) => {
                let v = e.eval(g, m, l, depth);
                if let Some((_, body)) = arms.iter().find(|(k, _)| *k == v) {
                    for s in body {
                        s.exec(g, m, l, depth);
                    }
                } else {
                    // Unreachable: the subject is a constant the generator put
                    // a label on. If it ever happens, the emitted program would
                    // take the same path, so saying so is better than silence.
                    panic!("generated case has no arm for {v}");
                }
            }
            Stmt::Call(f, args) => {
                let args: Vec<i32> = args.iter().map(|a| a.eval(g, m, l, depth)).collect();
                let _ = g.call(*f, &args, m, depth + 1);
            }
        }
    }

    fn render(&self, g: &Gen, loc: &[String], level: usize, out: &mut String) {
        let pad = "  ".repeat(level);
        match self {
            Stmt::SetM(i, e) => {
                let _ = writeln!(out, "{pad}{} := {};", g.mvars[*i], e.render(g, loc));
            }
            Stmt::SetL(i, e) => {
                let _ = writeln!(out, "{pad}{} := {};", loc[*i], e.render(g, loc));
            }
            Stmt::Block(body) => {
                for s in body {
                    s.render(g, loc, level, out);
                }
            }
            Stmt::If(c, then, els) => {
                let _ = writeln!(out, "{pad}IF {} THEN", c.render(g, loc));
                for s in then {
                    s.render(g, loc, level + 1, out);
                }
                if !els.is_empty() {
                    let _ = writeln!(out, "{pad}ELSE");
                    for s in els {
                        s.render(g, loc, level + 1, out);
                    }
                }
                let _ = writeln!(out, "{pad}END;");
            }
            Stmt::While(c, body) => {
                let _ = writeln!(out, "{pad}WHILE {} DO", c.render(g, loc));
                for s in body {
                    s.render(g, loc, level + 1, out);
                }
                let _ = writeln!(out, "{pad}END;");
            }
            Stmt::Repeat(body, c) => {
                let _ = writeln!(out, "{pad}REPEAT");
                for s in body {
                    s.render(g, loc, level + 1, out);
                }
                // `REPEAT … UNTIL expr` has no `END`: the report's production
                // is `RepeatStatement = REPEAT StatementSequence UNTIL
                // expression`, unlike Oberon-2's.
                let _ = writeln!(out, "{pad}UNTIL {};", c.render(g, loc));
            }
            Stmt::For(v, lo, hi, body) => {
                let _ = writeln!(out, "{pad}FOR {} := ({lo}) TO ({hi}) DO", g.mvars[*v]);
                for s in body {
                    s.render(g, loc, level + 1, out);
                }
                let _ = writeln!(out, "{pad}END;");
            }
            Stmt::Case(e, arms) => {
                let _ = writeln!(out, "{pad}CASE {} OF", e.render(g, loc));
                for (i, (k, body)) in arms.iter().enumerate() {
                    let sep = if i == 0 { " " } else { "|" };
                    let _ = writeln!(out, "{pad}  {sep} {k}:");
                    for s in body {
                        s.render(g, loc, level + 2, out);
                    }
                }
                let _ = writeln!(out, "{pad}END;");
            }
            Stmt::Call(f, args) => {
                let args: Vec<String> = args.iter().map(|a| a.render(g, loc)).collect();
                let _ = writeln!(out, "{pad}{}({});", g.funcs[*f].name, args.join(", "));
            }
        }
    }
}

// ------------------------------------------------------------ generator ---

/// A generated procedure. `locals[0]` is the accumulator of a function, and the
/// index space used by `Expr::LVar` is `params ++ locals`.
struct Func {
    name: String,
    params: Vec<String>,
    locals: Vec<String>,
    /// Assignments emitted before the body: every local starts defined, because
    /// the report leaves a variable undefined until it is assigned and C's
    /// zero-initialisation is not ours to rely on.
    prepare: Vec<Stmt>,
    body: Vec<Stmt>,
    /// `Some` for a function procedure, whose body ends in `RETURN acc`.
    is_function: bool,
}

impl Func {
    fn acc(&self) -> usize {
        self.params.len()
    }
}

struct Gen {
    rng: Rng,
    consts: Vec<(&'static str, i32)>,
    mvars: Vec<String>,
    funcs: Vec<Func>,
    /// The functions a generated expression may call: earlier ones only, since
    /// the report has no forward declarations.
    callable: Vec<usize>,
    /// Parameters plus locals of the procedure being generated, or 0 in the
    /// module body. It is what `Expr::LVar` may index.
    locals_in_scope: usize,
    /// Module variables no statement may assign, for the whole program: the
    /// counter of a generated loop (whose increment is written by hand) and a
    /// `FOR` control variable (read-only inside its loop, report §9.8). A
    /// procedure called from inside a loop must not be able to clobber it.
    frozen: Vec<bool>,
}

impl Gen {
    /// A fresh module variable, with its frozen flag: the two vectors must stay
    /// in step, and the only way to be sure is to push both here.
    fn new_var(&mut self, name: &str, frozen: bool) -> usize {
        let index = self.mvars.len();
        self.mvars.push(format!("{name}{index}"));
        self.frozen.push(frozen);
        index
    }

    /// Interprets a call, for the oracle.
    fn call(&self, f: usize, args: &[i32], m: &mut [i32], depth: u32) -> i32 {
        assert!(depth < 64, "recursion deeper than the generator emits");
        let func = &self.funcs[f];
        let mut l: Vec<i32> = args.to_vec();
        l.resize(func.params.len() + func.locals.len(), 0);
        for s in &func.prepare {
            s.exec(self, m, &mut l, depth);
        }
        for s in &func.body {
            s.exec(self, m, &mut l, depth);
        }
        if func.is_function {
            l[func.acc()]
        } else {
            0
        }
    }
}

// --------------------------------------------------------------- program ---

struct Program {
    g: Gen,
    body: Vec<Stmt>,
    /// When set, a self-check after *every* top-level body statement, for that
    /// variable: the first trap then names the statement that diverged, which is
    /// how a failing seed is triaged.
    per_statement: Option<usize>,
}

impl Program {
    /// The mirror's value of one module variable after each top-level statement
    /// of the module body, with the statement as Oberon. A failing seed names the
    /// variable (`(* check m1 expected 2 *)`), so this says which statement moved
    /// it away from that expectation.
    fn explain(&self, var: usize) -> Vec<(String, i32)> {
        let mut m = vec![0i32; self.g.mvars.len()];
        let mut l: Vec<i32> = Vec::new();
        let mut out = Vec::new();
        for s in &self.body {
            s.exec(&self.g, &mut m, &mut l, 0);
            let mut text = String::new();
            s.render(&self.g, &[], 1, &mut text);
            // One line per statement, however deeply nested its own text is.
            let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
            out.push((flat, m[var]));
        }
        out
    }

    /// The expected value of every module variable, from the mirror.
    fn expectations(&self) -> Vec<i32> {
        let mut m = vec![0i32; self.g.mvars.len()];
        let mut l: Vec<i32> = Vec::new();
        for s in &self.body {
            s.exec(&self.g, &mut m, &mut l, 0);
        }
        m
    }

    fn render(&self) -> String {
        let g = &self.g;
        let mut out = String::new();
        let _ = writeln!(out, "MODULE Fuzz;");

        if !g.consts.is_empty() {
            let _ = writeln!(out, "  CONST");
            for (name, value) in &g.consts {
                let _ = writeln!(out, "    {name} = {};", literal(*value));
            }
        }

        let _ = writeln!(out, "  VAR");
        for name in &g.mvars {
            let _ = writeln!(out, "    {name}: INTEGER;");
        }
        // The trap trigger: out of range for its own length, on purpose.
        let _ = writeln!(out, "    fail: ARRAY 1 OF INTEGER;");

        for f in &g.funcs {
            render_func(g, f, &mut out);
        }

        let _ = writeln!(out, "BEGIN");
        let trace = self.per_statement.map(|v| self.explain(v));
        // A check may only be emitted once the variable it reads has been
        // defined: the first statement that assigns it is that point (every
        // variable gets one — see `generate`).
        let first_def = self
            .per_statement
            .and_then(|v| {
                self.body
                    .iter()
                    .position(|s| matches!(s, Stmt::SetM(k, _) if *k == v))
            })
            .unwrap_or(0);
        for (i, s) in self.body.iter().enumerate() {
            s.render(g, &[], 1, &mut out);
            if let Some(trace) = &trace {
                let v = self.per_statement.expect("trace implies a variable");
                if let Some((_, value)) = trace.get(i).filter(|_| i >= first_def) {
                    let name = &g.mvars[v];
                    let _ = writeln!(
                        out,
                        "  IF {name} # {} THEN fail[1] := 0 END;  (* after stmt {i}: {name} expected {} *)",
                        literal(*value),
                        value
                    );
                }
            }
        }
        let expected = self.expectations();
        for (k, name) in g.mvars.iter().enumerate() {
            let _ = writeln!(
                out,
                "  IF {name} # {} THEN fail[1] := 0 END;  (* check {name} expected {} *)",
                literal(expected[k]),
                expected[k]
            );
        }
        let _ = writeln!(out, "END Fuzz.");
        out
    }
}

fn render_func(g: &Gen, f: &Func, out: &mut String) {
    let mut params: Vec<String> = Vec::new();
    for p in &f.params {
        params.push(format!("{p}: INTEGER"));
    }
    if f.is_function {
        let _ = writeln!(
            out,
            "  PROCEDURE {}({}): INTEGER;",
            f.name,
            params.join("; ")
        );
    } else {
        let _ = writeln!(out, "  PROCEDURE {}({});", f.name, params.join("; "));
    }
    if !f.locals.is_empty() {
        let _ = writeln!(out, "    VAR {}: INTEGER;", f.locals.join(", "));
    }
    let _ = writeln!(out, "  BEGIN");
    let loc = loc_of(f);
    for s in &f.prepare {
        s.render(g, &loc, 2, out);
    }
    for s in &f.body {
        s.render(g, &loc, 2, out);
    }
    if f.is_function {
        let _ = writeln!(out, "    RETURN {}", loc[f.acc()]);
    }
    let _ = writeln!(out, "  END {};", f.name);
    let _ = writeln!(out);
}

/// The emitted names of a procedure's parameters and locals, in the index space
/// `Expr::LVar` uses: parameters first, then locals (locals\[0\] is the
/// accumulator).
fn loc_of(f: &Func) -> Vec<String> {
    // The declared names, exactly as `render_func` writes them: parameters and
    // locals live in the procedure's own scope, so no prefixing is needed (and
    // none of them collides with a module variable, which is `m<k>`).
    let mut v = f.params.clone();
    v.extend(f.locals.iter().cloned());
    v
}

/// `MIN(INTEGER)` and `MAX(INTEGER)` are not Oberon-07 (report §10.2), and
/// `-2147483648` is not a legal literal (the integer token must fit an
/// `INTEGER`, DESIGN §4), so the extreme values are written out.
const MIN_EXPR: &str = "(0 - 2147483647 - 1)";
const MAX_EXPR: &str = "2147483647";

/// An `INTEGER` constant, spelled in a way Oberon accepts.
fn literal(k: i32) -> String {
    if k == i32::MIN {
        MIN_EXPR.to_string()
    } else if k < 0 {
        format!("({k})")
    } else {
        k.to_string()
    }
}

// -------------------------------------------------------- generation ------

fn generate(seed: u64) -> Program {
    let mut rng = Rng::new(seed);

    let mut consts = Vec::new();
    for name in ["c0", "c1", "c2"].iter().take(1 + rng.below(3)) {
        consts.push((*name, *rng.pick(&[0, 1, 2, 3, 7, -1, -5, 1000, i32::MAX])));
    }

    let nmvars = 3 + rng.below(4);
    let mvars: Vec<String> = (0..nmvars).map(|k| format!("m{k}")).collect();

    // One or two functions, optionally a proper procedure, and `R`, the
    // recursive one, which the module body always calls with a small argument.
    let mut funcs: Vec<Func> = Vec::new();
    for k in 0..1 + rng.below(2) {
        funcs.push(Func {
            name: format!("F{k}"),
            params: (0..1 + rng.below(2)).map(|p| format!("p{p}")).collect(),
            locals: vec!["acc".to_string()],
            prepare: Vec::new(),
            body: Vec::new(),
            is_function: true,
        });
    }
    if rng.below(2) == 0 {
        funcs.push(Func {
            name: "P0".to_string(),
            params: vec!["p0".to_string()],
            locals: Vec::new(),
            prepare: Vec::new(),
            body: Vec::new(),
            is_function: false,
        });
    }
    let r = funcs.len();
    funcs.push(Func {
        name: "R".to_string(),
        params: vec!["n".to_string()],
        locals: vec!["acc".to_string()],
        prepare: Vec::new(),
        body: Vec::new(),
        is_function: true,
    });

    let mut g = Gen {
        rng,
        consts,
        mvars,
        funcs,
        callable: Vec::new(),
        locals_in_scope: 0,
        frozen: vec![false; nmvars],
    };

    // The module body: define every variable, then a handful of statements. Each
    // initial value is a *literal*: the report leaves a variable undefined until
    // it is assigned, so an initialiser that read another variable would make the
    // mirror depend on the C back end's static zero-initialisation.
    let mut body: Vec<Stmt> = Vec::new();
    for k in 0..g.mvars.len() {
        let v = *g.rng.pick(&[0, 1, 2, -1, 7, 1000]);
        body.push(Stmt::SetM(k, Expr::Int(v)));
    }
    // Every procedure except `R`, which is reached only through the
    // hand-written recursive call below: a random argument would make both the
    // mirror and the real program recurse for an unbounded time.
    g.callable = (0..g.funcs.len())
        .filter(|f| g.funcs[*f].name != "R")
        .collect();
    g.locals_in_scope = 0;
    for _ in 0..4 + g.rng.below(7) {
        body.push(gen_stmt(&mut g, 3));
    }
    // Always exercise recursion: `R(3)` = 3 + 2 + 1 + 0.
    body.push(Stmt::SetM(
        0,
        Expr::Bin(
            Op::Add,
            Box::new(Expr::MVar(0)),
            Box::new(Expr::Call(r, vec![Expr::Int(3)])),
        ),
    ));

    // Procedure bodies. Each may call the *earlier* functions and itself.
    for f in 0..g.funcs.len() {
        let is_r = g.funcs[f].name == "R";
        let nparams = g.funcs[f].params.len();
        let nlocals = g.funcs[f].locals.len();
        // Earlier functions only, and never itself: the recursive call in `R`
        // is written by hand (a random one could recurse without a base case).
        g.callable = (0..f).filter(|k| g.funcs[*k].is_function).collect();
        g.locals_in_scope = nparams + nlocals;

        // Every parameter is bound by the caller; every local starts defined.
        let mut prepare: Vec<Stmt> = Vec::new();
        for k in nparams..nparams + nlocals {
            prepare.push(Stmt::SetL(k, Expr::Int(*g.rng.pick(&[0, 1, 2, -1, 7]))));
        }

        let mut fbody: Vec<Stmt> = Vec::new();
        if is_r {
            // `acc := <expr over n> + R(n - 1)`, guarded by `n > 0`: the only
            // recursion the generator emits, and it terminates.
            let e = gen_expr(&mut g, 2);
            fbody.push(Stmt::If(
                Cond::Rel(">", Expr::LVar(0), Expr::Int(0)),
                vec![Stmt::SetL(
                    nparams,
                    Expr::Bin(
                        Op::Add,
                        Box::new(e),
                        Box::new(Expr::Call(f, vec![Expr::Bin(
                            Op::Sub,
                            Box::new(Expr::LVar(0)),
                            Box::new(Expr::Int(1)),
                        )])),
                    ),
                )],
                Vec::new(),
            ));
        } else {
            for _ in 0..2 + g.rng.below(3) {
                fbody.push(gen_stmt(&mut g, 2));
            }
        }
        g.funcs[f].prepare = prepare;
        g.funcs[f].body = fbody;
    }

    // Loop counters and `FOR` controls are created while the body is generated,
    // and an assignment inside a loop can be skipped (an untaken arm) — so after
    // the fact, every variable the generator ended up with is defined up front,
    // with a literal. That removes the last place where the mirror and the C back
    // end agreed only because statics start at zero.
    // Counters and `FOR` controls are created *while* the body is generated, and
    // the assignment that defines one sits inside its loop — which an untaken arm
    // can skip. So once the variable list is final, every variable the generator
    // ended up with is defined up front (the ones with an initialiser already
    // keep their value). That removes the last place where the mirror and the C
    // back end agreed only because statics start at zero.
    let mut init: Vec<Stmt> = Vec::new();
    for k in nmvars..g.mvars.len() {
        init.push(Stmt::SetM(k, Expr::Int(0)));
    }
    init.append(&mut body);
    let body = init;
    Program {
        g,
        body,
        per_statement: None,
    }
}

/// A random expression. `depth` bounds the tree.
fn gen_expr(g: &mut Gen, depth: u32) -> Expr {
    let nlocals = g.locals_in_scope;
    let mut leaves: Vec<Expr> = vec![
        Expr::Int(*g.rng.pick(&[
            0,
            1,
            2,
            3,
            4,
            -1,
            -2,
            7,
            9,
            100,
            1000,
            i32::MIN,
        ])),
        Expr::MinInt,
        Expr::MaxInt,
    ];
    if !g.consts.is_empty() {
        leaves.push(Expr::Konst(g.rng.below(g.consts.len())));
    }
    for k in 0..g.mvars.len() {
        leaves.push(Expr::MVar(k));
    }
    for k in 0..nlocals {
        leaves.push(Expr::LVar(k));
    }
    if depth == 0 {
        return leaves[g.rng.below(leaves.len())].clone();
    }
    match g.rng.below(10) {
        0 => Expr::Neg(Box::new(gen_expr(g, depth - 1))),
        1 => Expr::Abs(Box::new(gen_expr(g, depth - 1))),
        2 => {
            let name = *g.rng.pick(&["LSL", "ASR", "ROR"]);
            let n = *g.rng.pick(&[0u32, 1, 3, 7, 15, 16, 31, 32, 33, 63]);
            Expr::Shift(name, Box::new(gen_expr(g, depth - 1)), n)
        }
        3 | 4 => {
            let functionals: Vec<usize> = g
                .callable
                .iter()
                .copied()
                .filter(|f| g.funcs[*f].is_function)
                .collect();
            if functionals.is_empty() {
                leaves[g.rng.below(leaves.len())].clone()
            } else {
                let f = *g.rng.pick(&functionals);
                let nargs = g.funcs[f].params.len();
                let args = (0..nargs).map(|_| gen_expr(g, depth - 1)).collect();
                Expr::Call(f, args)
            }
        }
        _ => {
            let op = *g.rng.pick(&[
                Op::Add,
                Op::Sub,
                Op::Mul,
                Op::Add,
                Op::Sub,
                Op::Mul,
                Op::Div,
                Op::Mod,
            ]);
            let left = gen_expr(g, depth - 1);
            let right = if matches!(op, Op::Div | Op::Mod) {
                Expr::Int(1 + g.rng.below(9) as i32)
            } else {
                gen_expr(g, depth - 1)
            };
            Expr::Bin(op, Box::new(left), Box::new(right))
        }
    }
}

fn gen_cond(g: &mut Gen, depth: u32) -> Cond {
    if depth == 0 || g.rng.below(4) != 0 {
        let op = *g.rng.pick(&["=", "#", "<", "<=", ">", ">="]);
        Cond::Rel(op, gen_expr(g, 1), gen_expr(g, 1))
    } else if g.rng.below(2) == 0 {
        Cond::Not(Box::new(gen_cond(g, depth - 1)))
    } else {
        Cond::And(
            Box::new(gen_cond(g, depth - 1)),
            Box::new(gen_cond(g, depth - 1)),
        )
    }
}

/// One statement. `frozen` module variables (loop counters and `FOR` control
/// variables) are never assignable.
fn gen_stmt(g: &mut Gen, depth: u32) -> Stmt {
    let nlocals = g.locals_in_scope;
    let assignable: Vec<usize> = (0..g.mvars.len())
        .filter(|k| !g.frozen[*k])
        .collect();
    let kind = if depth == 0 { 0 } else { g.rng.below(10) };
    match kind {
        0..=2 => {
            let to_local = nlocals > 0 && g.rng.below(2) == 0;
            let e = gen_expr(g, 3);
            if to_local {
                Stmt::SetL(g.rng.below(nlocals), e)
            } else if assignable.is_empty() {
                if nlocals > 0 {
                    Stmt::SetL(g.rng.below(nlocals), e)
                } else {
                    Stmt::Block(Vec::new())
                }
            } else {
                Stmt::SetM(*g.rng.pick(&assignable), e)
            }
        }
        3 => {
            let c = gen_cond(g, 1);
            let then = (0..1 + g.rng.below(2))
                .map(|_| gen_stmt(g, depth - 1))
                .collect();
            let els = (0..1 + g.rng.below(2))
                .map(|_| gen_stmt(g, depth - 1))
                .collect();
            Stmt::If(c, then, els)
        }
        4 => {
            // `counter := 0; WHILE counter < k DO … counter := counter + 1 END`:
            // termination by construction, which the mirror relies on too.
            let counter = g.new_var("m", true);
            let k = g.rng.range(1, 4);
            let mut body: Vec<Stmt> = (0..1 + g.rng.below(2))
                .map(|_| gen_stmt(g, depth - 1))
                .collect();
            body.push(Stmt::SetM(
                counter,
                Expr::Bin(Op::Add, Box::new(Expr::MVar(counter)), Box::new(Expr::Int(1))),
            ));
            Stmt::Block(vec![
                Stmt::SetM(counter, Expr::Int(0)),
                Stmt::While(Cond::Rel("<", Expr::MVar(counter), Expr::Int(k)), body),
            ])
        }
        5 => {
            let counter = g.new_var("m", true);
            let k = g.rng.range(1, 4);
            let mut body: Vec<Stmt> = (0..1 + g.rng.below(2))
                .map(|_| gen_stmt(g, depth - 1))
                .collect();
            body.push(Stmt::SetM(
                counter,
                Expr::Bin(Op::Add, Box::new(Expr::MVar(counter)), Box::new(Expr::Int(1))),
            ));
            Stmt::Block(vec![
                Stmt::SetM(counter, Expr::Int(0)),
                Stmt::Repeat(body, Cond::Rel(">=", Expr::MVar(counter), Expr::Int(k))),
            ])
        }
        6 => {
            let v = g.new_var("m", true);
            let lo = g.rng.range(-2, 2);
            let hi = lo + g.rng.range(0, 3);
            let body = (0..1 + g.rng.below(2))
                .map(|_| gen_stmt(g, depth - 1))
                .collect();
            Stmt::For(v, lo, hi, body)
        }
        7 => {
            // The subject is a constant, so the generator knows which label it
            // matches and can guarantee a match without a default arm. Labels
            // are `integer | string | qualident` (report appendix): non-negative
            // integers, all distinct.
            let subject = g.rng.below(4);
            let mut labels: Vec<usize> = vec![subject];
            for k in 0..4 {
                if k != subject && g.rng.below(2) == 0 {
                    labels.push(k);
                }
            }
            let arms: Vec<(i32, Vec<Stmt>)> = labels
                .into_iter()
                .map(|k| {
                    let body = (0..1 + g.rng.below(2))
                        .map(|_| gen_stmt(g, depth - 1))
                        .collect();
                    (k as i32, body)
                })
                .collect();
            Stmt::Case(Expr::Int(subject as i32), arms)
        }
        _ => {
            // A call to a proper procedure, if one was generated. Otherwise an
            // assignment, which is always available: the module has variables.
            let proper = (0..g.funcs.len())
                .find(|f| !g.funcs[*f].is_function && g.callable.contains(f));
            match proper {
                Some(f) => {
                    let n = g.funcs[f].params.len();
                    let args = (0..n).map(|_| gen_expr(g, 2)).collect();
                    Stmt::Call(f, args)
                }
                None if !assignable.is_empty() => {
                    let e = gen_expr(g, 2);
                    Stmt::SetM(*g.rng.pick(&assignable), e)
                }
                None => Stmt::Block(Vec::new()),
            }
        }
    }
}

// --------------------------------------------------------------- runner ---

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut seeds = 25u64;
    let mut base = 1u64;
    let mut print_only = false;
    let mut explain: Option<String> = None;
    let mut per_statement: Option<String> = None;
    let mut obc = PathBuf::from("target/debug/obc");
    let mut work = std::env::temp_dir().join("obgen");
    let mut keep = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--seeds" => {
                i += 1;
                seeds = args[i].parse().expect("--seeds N");
            }
            "--base" => {
                i += 1;
                base = args[i].parse().expect("--base N");
            }
            "--obc" => {
                i += 1;
                obc = PathBuf::from(&args[i]);
            }
            "--work" => {
                i += 1;
                work = PathBuf::from(&args[i]);
            }
            "--print" => print_only = true,
            "--per-statement" => {
                i += 1;
                per_statement = Some(args[i].clone());
            }
            "--explain" => {
                i += 1;
                explain = Some(args[i].clone());
            }
            "--keep" => keep = true,
            other => {
                eprintln!("obgen: unknown argument {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }

    if let Some(var) = &explain {
        let program = generate(base);
        let index = program
            .g
            .mvars
            .iter()
            .position(|n| n == var)
            .unwrap_or_else(|| panic!("no variable named `{var}` in seed {base}"));
        for (text, value) in program.explain(index) {
            println!("{value:>12}  {text}");
        }
        return;
    }
    if let Some(var) = &per_statement {
        let mut program = generate(base);
        program.per_statement = Some(
            program
                .g
                .mvars
                .iter()
                .position(|n| n == var)
                .unwrap_or_else(|| panic!("no variable named `{var}` in seed {base}")),
        );
        print!("{}", program.render());
        return;
    }
    if print_only {
        print!("{}", generate(base).render());
        return;
    }
    std::process::exit(run(base, seeds, &obc, &work, keep));
}

fn run(base: u64, seeds: u64, obc: &Path, work: &Path, keep: bool) -> i32 {
    std::fs::create_dir_all(work).expect("cannot create the work directory");
    let mut failed = 0;
    for seed in base..base + seeds {
        match one(seed, obc, work, keep) {
            Ok(()) => println!("PASS seed-{seed}"),
            Err(why) => {
                println!("FAIL seed-{seed}: {why}");
                failed += 1;
            }
        }
    }
    println!(
        "SUMMARY obgen: {seeds} seed(s), {} passed, {failed} failed",
        seeds - failed
    );
    if failed == 0 {
        0
    } else {
        1
    }
}

fn one(seed: u64, obc: &Path, work: &Path, keep: bool) -> Result<(), String> {
    let program = generate(seed);
    let src = program.render();
    let dir = work.join(format!("seed-{seed}"));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("Fuzz.mod");
    std::fs::write(&path, &src).map_err(|e| e.to_string())?;
    // A failing seed is the artefact worth keeping: drop the directory of a
    // passing one unless the caller asked for everything.
    let drop_dir = || {
        if !keep {
            let _ = std::fs::remove_dir_all(&dir);
        }
    };

    let out = Command::new(obc)
        .arg("build")
        .arg(&path)
        .arg("--out")
        .arg(&dir)
        .arg("-o")
        .arg(dir.join("Fuzz"))
        .output()
        .map_err(|e| format!("cannot run {}: {e}", obc.display()))?;
    if !out.status.success() {
        let log = String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "the compiler rejected a generated program\n{log}  source: {}",
            path.display()
        ));
    }

    let exe = dir.join("Fuzz");
    let run = Command::new("timeout")
        .arg("10")
        .arg(&exe)
        .output()
        .map_err(|e| format!("cannot run {}: {e}", exe.display()))?;
    if run.status.success() {
        drop_dir();
        return Ok(());
    }
    let err = String::from_utf8_lossy(&run.stderr).to_string();
    // A trap names the byte offset of the designator, and the check line names
    // the variable and the expectation, so the failure explains itself.
    // The runtime prints `index out of range (at byte N of the source)`, so take
    // the digits after "at byte " rather than looking for a closing bracket.
    let at = err
        .split("at byte ")
        .nth(1)
        .map(|s| {
            s.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
        })
        .and_then(|s| s.parse::<u32>().ok());
    let mut why = format!("exited with {}", run.status);
    if let Some(at) = at {
        let (line, col) = line_col(&src, at);
        let text = src.lines().nth(line as usize - 1).unwrap_or("");
        let _ = write!(why, "; trapped at {line}:{col}: {}", text.trim());
    } else if !err.trim().is_empty() {
        let _ = write!(why, "; stderr: {}", err.trim());
    }
    Err(why)
}
