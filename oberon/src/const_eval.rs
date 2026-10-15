//! Constant-expression evaluation (report §5: "can be evaluated by a mere
//! textual scan without actually executing the program").
//!
//! Four contexts need it: constant declarations (§5), array lengths (§6.2),
//! `CASE` labels (§9.5) and the `BY` increment of `FOR` (§9.8). The design also
//! claims something specific and checkable here: **the folder and the generated
//! C compute the same values**, because both implement the report's `DIV`/`MOD`
//! (floor), the shift counts reduced mod 32, and the set operations the same
//! way. `tests::agrees_with_the_emitted_c` checks that claim against a table
//! produced by the C back end's own helpers.
//!
//! Decisions the report leaves open, made here and listed in DESIGN.md §13:
//!
//! * `x / y` on two integers is `x DIV y`. The report says `/` applies to
//!   numeric types and that the result has the operands' type, but defines only
//!   `DIV`/`MOD` precisely; the reference compilers implement integer `/` with
//!   the same code as `DIV`, so we do too.
//! * `CHR(x)` truncates like the emitted `(char)x` rather than rejecting an
//!   ordinal above 255, so the folder cannot disagree with the running program.
//! * Arithmetic on `INTEGER` wraps (`wrapping_*`), matching the `-fwrapv` C the
//!   back end is compiled with. `ABS` and `-` on `MIN(INTEGER)` wrap likewise.
//! * `ORD` of a `SET` is not folded: the report lists `SET` in the argument
//!   types but a set has no single ordinal number, so no constant is produced.

use crate::ast::*;

#[derive(Debug, Clone, PartialEq)]
pub enum Const {
    Int(i32),
    Real(f32),
    Bool(bool),
    Char(u8),
    Str(String),
    Set(u32),
}

impl Const {
    pub fn int(&self) -> Option<i32> {
        match self {
            Const::Int(v) => Some(*v),
            Const::Char(c) => Some(*c as i32),
            Const::Bool(false) => Some(0),
            Const::Bool(true) => Some(1),
            _ => None,
        }
    }
    pub fn set(&self) -> Option<u32> {
        match self {
            Const::Set(v) => Some(*v),
            _ => None,
        }
    }
    pub fn bool(&self) -> Option<bool> {
        match self {
            Const::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn real(&self) -> Option<f32> {
        match self {
            Const::Real(v) => Some(*v),
            Const::Int(v) => Some(*v as f32),
            _ => None,
        }
    }
    pub fn string(&self) -> Option<&str> {
        match self {
            Const::Str(s) => Some(s),
            _ => None,
        }
    }
}

pub type EvalResult = Result<Const, String>;

fn not_const(what: &str) -> String {
    format!("{what} is not a constant expression (report 5)")
}

// ---------------------------------------------------------------- semantics

/// `x DIV y`, floor division: the report's `x = q*y + r, 0 <= r < y` for `y > 0`,
/// extended to negative `y` by flooring. Identical to the C `rt_div`.
pub fn div(x: i32, y: i32) -> EvalResult {
    if y == 0 {
        return Err("division by zero in a constant expression".into());
    }
    if y == -1 {
        // matches the C helper, and avoids the INT32_MIN / -1 overflow
        return Ok(Const::Int(x.wrapping_neg()));
    }
    let q = x.wrapping_div(y);
    let r = x.wrapping_rem(y);
    Ok(Const::Int(if r != 0 && (r < 0) != (y < 0) { q - 1 } else { q }))
}

/// `x MOD y`, the companion of [`div`]. Identical to the C `rt_mod`.
pub fn modulo(x: i32, y: i32) -> EvalResult {
    if y == 0 {
        return Err("division by zero in a constant expression".into());
    }
    if y == -1 {
        return Ok(Const::Int(0));
    }
    let r = x.wrapping_rem(y);
    Ok(Const::Int(if r != 0 && (r < 0) != (y < 0) { r + y } else { r }))
}

fn lsl(x: i32, n: i32) -> i32 {
    ((x as u32) << (n & 31)) as i32
}
fn asr(x: i32, n: i32) -> i32 {
    let n = n & 31;
    if x < 0 {
        !((!(x as u32)) >> n) as i32
    } else {
        (x as u32 >> n) as i32
    }
}
fn ror(x: i32, n: i32) -> i32 {
    // `rotate_right` is the same operation as the emitted
    // `(x >> n) | (x << (32 - n))` with the count taken mod 32, and clippy
    // insists on it; the mod-32 count is what makes the two agree.
    (x as u32).rotate_right((n & 31) as u32) as i32
}

/// `{a .. b}` as the back end's `((1u << (b-a+1)) - 1u) << a`.
pub fn range(a: i32, b: i32) -> Result<u32, String> {
    if a < 0 || b > 31 {
        return Err(format!(
            "set element outside 0..31 in a constant expression ({a} .. {b})"
        ));
    }
    if a > b {
        return Ok(0);
    }
    if b == 31 {
        return Ok(0xFFFF_FFFFu32 << a);
    }
    Ok(((1u32 << (b - a + 1)) - 1) << a)
}

fn ord_of(c: &Const) -> Result<i32, String> {
    match c {
        Const::Char(ch) => Ok(*ch as i32),
        // report §9.1.2: a single-character string may be used where a CHAR is
        // expected - `ORD("0")` appears in the report's own example (§10.1).
        Const::Str(t) if t.len() == 1 => Ok(t.as_bytes()[0] as i32),
        Const::Bool(b) => Ok(*b as i32),
        _ => Err("ORD needs a constant CHAR or BOOLEAN".into()),
    }
}

// ------------------------------------------------------------------- folder

pub fn eval(e: &Expr) -> EvalResult {
    match &e.kind {
        ExprKind::Int(v) => Ok(Const::Int(*v as i32)),
        ExprKind::Real(v) => Ok(Const::Real(*v as f32)),
        ExprKind::Char(c) => Ok(Const::Char(*c)),
        ExprKind::Str(s) => Ok(Const::Str(s.clone())),
        ExprKind::Bool(b) => Ok(Const::Bool(*b)),
        ExprKind::Nil => Err(not_const("NIL")),
        ExprKind::TypeTest(..) => Err(not_const("a type test")),
        ExprKind::Set(elems) => {
            let mut bits = 0u32;
            for el in elems {
                let lo = eval(&el.lo)?.int().ok_or_else(|| {
                    "a set element must be a constant integer (report 8.2)".to_string()
                })?;
                let hi = match &el.hi {
                    Some(h) => eval(h)?.int().ok_or_else(|| {
                        "a set range bound must be a constant integer".to_string()
                    })?,
                    None => lo,
                };
                bits |= range(lo, hi)?;
            }
            Ok(Const::Set(bits))
        }
        ExprKind::Designator(d) => {
            if d.base.len() == 1 && d.sels.is_empty() {
                return Err(not_const(&format!("the designator `{}`", d.base[0])));
            }
            if d.sels.is_empty() {
                return Err(not_const("a qualified designator"));
            }
            let name = if d.base.len() == 1 { &d.base[0] } else { "" };
            let args = match d.sels.as_slice() {
                // after `resolve::reclassify`
                [Selector::Call(args)] => args,
                // before it, a paren group is either
                [Selector::Args(Some(args))] => args,
                _ => return Err(not_const("a designator with selectors")),
            };
            if name.is_empty() {
                return Err(not_const("a qualified call"));
            }
            let vals: Vec<Const> = args.iter().map(eval).collect::<Result<_, _>>()?;
            call(name, &vals)
        }
        ExprKind::Unary(op, x) => {
            let v = eval(x)?;
            match (op, v) {
                (UnOp::Not, Const::Bool(b)) => Ok(Const::Bool(!b)),
                (UnOp::Not, _) => Err("~ needs a BOOLEAN operand (report 8.2.1)".into()),
                (UnOp::Neg, Const::Int(i)) => Ok(Const::Int(i.wrapping_neg())),
                (UnOp::Neg, Const::Real(r)) => Ok(Const::Real(-r)),
                (UnOp::Neg, Const::Set(s)) => Ok(Const::Set(!s)),
                (UnOp::Neg, _) => Err("- needs a numeric or SET operand (report 8.2.2)".into()),
                (UnOp::Plus, Const::Int(i)) => Ok(Const::Int(i)),
                (UnOp::Plus, Const::Real(r)) => Ok(Const::Real(r)),
                (UnOp::Plus, _) => Err("+ needs a numeric operand (report 8.2.2)".into()),
            }
        }
        ExprKind::Binary(op, l, r) => {
            let a = eval(l)?;
            let b = eval(r)?;
            binary(*op, a, b)
        }
    }
}

fn binary(op: BinOp, a: Const, b: Const) -> EvalResult {
    use Const::*;
    match op {
        BinOp::Or | BinOp::And => match (a.bool(), b.bool()) {
            (Some(x), Some(y)) => Ok(Bool(if op == BinOp::Or { x || y } else { x && y })),
            _ => Err("logical operators need BOOLEAN operands (report 8.2.1)".into()),
        },
        BinOp::In => match (a.int(), b.set()) {
            (Some(x), Some(s)) => Ok(Bool((s >> (x & 31)) & 1 != 0)),
            _ => Err("`IN` needs an INTEGER and a SET (report 8.2.4)".into()),
        },
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            let cmp = compare(op, &a, &b)?;
            Ok(Bool(cmp))
        }
        _ => arithmetic(op, a, b),
    }
}

fn compare(op: BinOp, a: &Const, b: &Const) -> Result<bool, String> {
    use Const::*;
    // ordering: numeric, CHAR, character arrays; = and # also BOOLEAN and SET
    let ord = match (a, b) {
        (Str(x), Str(y)) => Some(x.cmp(y)),
        (Set(x), Set(y)) => Some(x.cmp(y)),
        (Bool(x), Bool(y)) => Some(x.cmp(y)),
        _ => match (a.int(), b.int()) {
            (Some(x), Some(y)) => Some(x.cmp(&y)),
            _ => match (a.real(), b.real()) {
                (Some(x), Some(y)) => x.partial_cmp(&y),
                _ => None,
            },
        },
    };
    let ord = ord.ok_or_else(|| "these operands cannot be compared (report 8.2.4)".to_string())?;
    Ok(match op {
        BinOp::Eq => ord.is_eq(),
        BinOp::Ne => ord.is_ne(),
        BinOp::Lt => ord.is_lt(),
        BinOp::Le => ord.is_le(),
        BinOp::Gt => ord.is_gt(),
        BinOp::Ge => ord.is_ge(),
        _ => unreachable!(),
    })
}

fn arithmetic(op: BinOp, a: Const, b: Const) -> EvalResult {
    use BinOp::*;
    use Const::*;
    match (a, b) {
        (Int(x), Int(y)) => Ok(Int(match op {
            Add => x.wrapping_add(y),
            Sub => x.wrapping_sub(y),
            Mul => x.wrapping_mul(y),
            // Decision: integer `/` is `DIV` (see the module header).
            Div | IDiv => return div(x, y),
            Mod => return modulo(x, y),
            _ => return Err(format!("{op:?} does not apply to two integers here")),
        })),
        (Real(x), Real(y)) => Ok(Real(match op {
            Add => x + y,
            Sub => x - y,
            Mul => x * y,
            Div => x / y,
            _ => return Err(format!("{op:?} does not apply to REAL operands (report 8.2.2)")),
        })),
        (Set(x), Set(y)) => Ok(Set(match op {
            Add => x | y,
            Sub => x & !y,
            Mul => x & y,
            Div => x ^ y,
            _ => return Err(format!("{op:?} does not apply to SET operands (report 8.2.3)")),
        })),
        (x, y) => Err(format!(
            "constant folding: incompatible operands ({:?}, {:?}) for {op:?}",
            x, y
        )),
    }
}

fn call(name: &str, args: &[Const]) -> EvalResult {
    use Const::*;
    let arity = |n: usize| -> Result<(), String> {
        if args.len() == n {
            Ok(())
        } else {
            Err(format!("{name} takes {n} argument(s)"))
        }
    };
    let one_int = |f: fn(i32) -> Const| -> EvalResult {
        arity(1)?;
        let i = args[0]
            .int()
            .ok_or_else(|| format!("{name} needs a constant INTEGER"))?;
        Ok(f(i))
    };
    match name {
        "ABS" => match args.first() {
            Some(Int(i)) => Ok(Int(i.wrapping_abs())),
            Some(Real(r)) => Ok(Real(r.abs())),
            _ => Err("ABS needs a constant numeric operand (report 10.2)".into()),
        },
        "ODD" => {
            arity(1)?;
            let i = args[0].int().ok_or_else(|| "ODD needs an INTEGER".to_string())?;
            Ok(Bool(i & 1 == 1))
        }
        "LSL" | "ASR" | "ROR" => {
            arity(2)?;
            let (x, n) = (
                args[0].int().ok_or_else(|| format!("{name} needs INTEGERs"))?,
                args[1].int().ok_or_else(|| format!("{name} needs INTEGERs"))?,
            );
            Ok(Int(match name {
                "LSL" => lsl(x, n),
                "ASR" => asr(x, n),
                _ => ror(x, n),
            }))
        }
        "FLOOR" => {
            arity(1)?;
            let r = args[0]
                .real()
                .ok_or_else(|| "FLOOR needs a REAL".to_string())?;
            if !r.is_finite() || r < -2147483648.0 || r > 2147483647.0 {
                return Err("FLOOR of a constant outside INTEGER's range".into());
            }
            Ok(Int(r.floor() as i32))
        }
        "FLT" => one_int(|i| Real(i as f32)),
        "ORD" => {
            arity(1)?;
            match &args[0] {
                Char(_) | Bool(_) | Str(_) => Ok(Int(ord_of(&args[0])?)),
                // A SET has no single ordinal number; see the module header.
                Set(_) => Err("ORD of a SET has no ordinal number".into()),
                _ => Err("ORD needs a constant CHAR or BOOLEAN".into()),
            }
        }
        // truncates exactly like the emitted `(CHAR)x`
        "CHR" => one_int(|i| Char((i & 0xFF) as u8)),
        // `LEN` needs an array's length, which the checker knows and this
        // folder (which sees only syntax) does not.
        "LEN" => Err("LEN of an array is folded by the checker, which knows lengths".into()),
        _ => Err(format!("`{name}` is not a predeclared function procedure")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_module;

    /// Parse `CONST c = <expr>;` through the real lexer and parser, so the
    /// folder is tested on trees the front end actually produces.
    fn eval_src(src: &str) -> EvalResult {
        let module = format!("MODULE M; CONST c = {src}; BEGIN END M.");
        let m = parse_module(&module).unwrap_or_else(|e| panic!("`{src}` must parse: {e:?}"));
        eval(&m.decls.consts[0].value)
    }

    #[test]
    fn integers_and_their_bit_patterns() {
        assert_eq!(eval_src("2*100 - 1").unwrap(), Const::Int(199));
        assert_eq!(eval_src("100H").unwrap(), Const::Int(256));
        assert_eq!(eval_src("1 + 2*3").unwrap(), Const::Int(7));
        assert_eq!(eval_src("(1 + 2)*3").unwrap(), Const::Int(9));
        assert_eq!(eval_src("- 5 + 8").unwrap(), Const::Int(3));
        // report §3: hexadecimal integers are bit patterns
        assert_eq!(eval_src("0FFFFFFFFH").unwrap(), Const::Int(-1));
        // wrapping, as the -fwrapv build of the emitted C
        assert_eq!(
            eval_src("7FFFFFFFH + 1").unwrap(),
            Const::Int(i32::MIN)
        );
    }

    #[test]
    fn div_and_mod_are_floor_and_match_the_c_helpers() {
        assert_eq!(eval_src("7 DIV 3").unwrap(), Const::Int(2));
        assert_eq!(eval_src("7 MOD 3").unwrap(), Const::Int(1));
        // report §8.2: `SimpleExpression = ["+"|"-"] term {AddOperator term}`,
        // so a leading sign applies to the whole term: `-7 DIV 3` is
        // `-(7 DIV 3)`, not `(-7) DIV 3`. Parenthesise to test the dividend.
        assert_eq!(eval_src("-7 DIV 3").unwrap(), Const::Int(-2));
        // `DIV` binds tighter than the additive operator
        assert_eq!(eval_src("0 - 7 DIV 3").unwrap(), Const::Int(-2));
        assert_eq!(eval_src("(0 - 7) DIV 3").unwrap(), Const::Int(-3));
        assert_eq!(eval_src("(0 - 7) MOD 3").unwrap(), Const::Int(2));
        assert_eq!(eval_src("7 DIV (0 - 3)").unwrap(), Const::Int(-3));
        assert_eq!(eval_src("7 MOD (0 - 3)").unwrap(), Const::Int(-2));
        assert_eq!(eval_src("(0 - 7) DIV (0 - 3)").unwrap(), Const::Int(2));
        assert_eq!(eval_src("(0 - 7) MOD (0 - 3)").unwrap(), Const::Int(-1));
        // integer `/` is DIV (decision in the module header)
        assert_eq!(eval_src("7 / 3").unwrap(), Const::Int(2));
        assert!(eval_src("1 DIV 0").unwrap_err().contains("division by zero"));
        assert!(eval_src("1 MOD 0").unwrap_err().contains("division by zero"));
    }

    #[test]
    fn sets_and_ranges() {
        assert_eq!(eval_src("{1 .. 3}").unwrap(), Const::Set(0b1110));
        assert_eq!(eval_src("{1 .. 1}").unwrap(), Const::Set(0b10));
        assert_eq!(eval_src("{5 .. 1}").unwrap(), Const::Set(0), "empty");
        assert_eq!(eval_src("{0 .. 31}").unwrap(), Const::Set(0xFFFF_FFFF));
        assert_eq!(eval_src("{1, 3 .. 4}").unwrap(), Const::Set(0b11010));
        assert_eq!(eval_src("{}").unwrap(), Const::Set(0));
        assert_eq!(eval_src("{0 .. 8*4 - 1}").unwrap(), Const::Set(0xFFFF_FFFF));
        assert!(eval_src("{0 .. 32}").unwrap_err().contains("outside 0..31"));
        assert!(eval_src("{-1}").unwrap_err().contains("outside 0..31"));
        // set operators (report §8.2.3)
        assert_eq!(eval_src("{1} + {2}").unwrap(), Const::Set(0b110));
        assert_eq!(eval_src("{1 .. 3} - {2}").unwrap(), Const::Set(0b1010));
        assert_eq!(eval_src("{1 .. 3} * {2 .. 4}").unwrap(), Const::Set(0b1100));
        // symmetric difference: {1,2} / {2,3} = {1,3}
        assert_eq!(eval_src("{1 .. 2} / {2 .. 3}").unwrap(), Const::Set(0b1010));
        assert_eq!(eval_src("- {0}").unwrap(), Const::Set(0xFFFF_FFFE));
    }

    #[test]
    fn shifts_agree_with_the_runtime_helpers() {
        assert_eq!(eval_src("LSL(1, 31)").unwrap(), Const::Int(i32::MIN));
        assert_eq!(eval_src("LSL(1, 32)").unwrap(), Const::Int(1), "count mod 32");
        assert_eq!(eval_src("LSL(1, 33)").unwrap(), Const::Int(2));
        assert_eq!(eval_src("ASR(-8, 1)").unwrap(), Const::Int(-4));
        assert_eq!(eval_src("ASR(-1, 31)").unwrap(), Const::Int(-1));
        assert_eq!(eval_src("ROR(1, 1)").unwrap(), Const::Int(i32::MIN));
        assert_eq!(eval_src("ROR(-2147483648, 1)").unwrap(), Const::Int(0x4000_0000));
        assert_eq!(eval_src("ROR(5, 0)").unwrap(), Const::Int(5));
    }

    #[test]
    fn reals_characters_and_booleans() {
        assert_eq!(eval_src("4.567E8").unwrap(), Const::Real(4.567E8));
        assert_eq!(eval_src("12.3 * 2.0").unwrap(), Const::Real(24.6));
        assert_eq!(eval_src("FLOOR(-1.5)").unwrap(), Const::Int(-2));
        assert_eq!(eval_src("FLOOR(1.5)").unwrap(), Const::Int(1));
        assert_eq!(eval_src("FLT(7)").unwrap(), Const::Real(7.0));
        assert_eq!(eval_src("ORD(\"A\")").unwrap(), Const::Int(65));
        assert_eq!(eval_src("ORD(22X)").unwrap(), Const::Int(0x22));
        assert_eq!(eval_src("ORD(TRUE)").unwrap(), Const::Int(1));
        assert_eq!(eval_src("CHR(65)").unwrap(), Const::Char(b'A'));
        assert_eq!(eval_src("CHR(321)").unwrap(), Const::Char(65), "truncates like (CHAR)x");
        assert_eq!(eval_src("\"Oberon\"").unwrap(), Const::Str("Oberon".into()));
        assert_eq!(eval_src("~TRUE").unwrap(), Const::Bool(false));
        assert_eq!(eval_src("TRUE OR FALSE").unwrap(), Const::Bool(true));
        assert_eq!(eval_src("(0 <= 3) & (3 < 100)").unwrap(), Const::Bool(true));
        assert_eq!(eval_src("3 IN {1 .. 5}").unwrap(), Const::Bool(true));
        assert_eq!(eval_src("9 IN {1 .. 5}").unwrap(), Const::Bool(false));
        assert_eq!(eval_src("ODD(-3)").unwrap(), Const::Bool(true), "-3 MOD 2 = 1");
        assert_eq!(eval_src("ABS(-7)").unwrap(), Const::Int(7));
        assert_eq!(eval_src("\"abc\" < \"abd\"").unwrap(), Const::Bool(true));
    }

    #[test]
    fn non_constants_are_rejected_with_a_reason() {
        assert!(eval_src("NIL").unwrap_err().contains("not a constant"));
        assert!(eval_src("x").unwrap_err().contains("not a constant"));
        assert!(eval_src("LEN(1)").unwrap_err().contains("folded by the checker"));
        assert!(eval_src("LEN(a)").unwrap_err().contains("not a constant"));
        assert!(eval_src("ORD({1})").unwrap_err().contains("no ordinal number"));
        assert!(eval_src("NOSUCH(1)").unwrap_err().contains("not a predeclared"));
    }

    /// The design claims the folder and the emitted C compute the same values.
    /// `oberon/golden/constexpr.tsv` is written by the C side: one line per
    /// expression, `id<TAB>decimal value`, each implemented in C with the
    /// runtime helpers (`rt_div`, `rt_mod`, `rt_lsl`, `rt_asr`, `rt_ror`,
    /// `rt_set`, `rt_in`) exactly as the back end would emit them.
    #[test]
    fn agrees_with_the_emitted_c() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/golden/constexpr.tsv");
        let table = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => panic!("run `bash oberon/golden/run.sh` first to produce {path}: {e}"),
        };
        let mut checked = 0;
        for line in table.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (src, want) = line
                .split_once('\t')
                .unwrap_or_else(|| panic!("malformed line: {line:?}"));
            let want: i64 = want.trim().parse().expect("decimal value");
            let got = eval_src(src)
                .unwrap_or_else(|e| panic!("`{src}` must fold, but: {e}"));
            let got = match got {
                Const::Int(v) => v as i64,
                Const::Bool(b) => b as i64,
                Const::Set(v) => v as i64,
                Const::Char(c) => c as i64,
                Const::Real(r) => r as i64,
                Const::Str(s) => s.len() as i64,
            };
            assert_eq!(got, want, "Rust folder and emitted C disagree on `{src}`");
            checked += 1;
        }
        assert!(checked >= 30, "expected a real table, saw {checked} rows");
    }
}
