//! Deterministic, self-oracled subset of obgen for the new standalone
//! compiler. This reuses obgen's seeded PRNG but deliberately emits only
//! INTEGER value procedures, locals, assignments, IF/ELSIF/ELSE and WHILE.
 //! Source, native C harness and C/Cranelift self-checking module are the
//! *same procedure*. No behavior is learned from any compiler backend.
use super::Rng;
use std::fmt::Write as _;

#[derive(Clone, Debug)]
struct Model {
    coefficients: [i32; 12],
    conditions: [&'static str; 4],
}
const COMPARISONS: [&str; 6] = [">", ">=", "<", "<=", "=", "#"];

fn make(seed: u64) -> Model {
    let mut rng = Rng::new(seed);
    let mut coefficients = [0; 12];
    for coefficient in &mut coefficients {
        *coefficient = rng.range(1, 9);
    }
    let conditions = [
        COMPARISONS[rng.below(COMPARISONS.len())],
        COMPARISONS[rng.below(COMPARISONS.len())],
        COMPARISONS[rng.below(COMPARISONS.len())],
        COMPARISONS[rng.below(COMPARISONS.len())],
    ];
    Model {coefficients,conditions}
}

fn compare(op: &str, a: i32, b: i32) -> bool {
    match op {
        ">" => a > b,
        ">=" => a >= b,
        "<" => a < b,
        "<=" => a <= b,
        "=" => a == b,
        "#" => a != b,
        _ => unreachable!("generated comparison operator"),
    }
}

/// Interpretation of the same generated program: independent of C, CLIF,
/// standalone compiler and all backend optimizations. INTEGER wraps at 32 bits.
fn expected(model: &Model, a: i32, b: i32, c: i32) -> i32 {
    let k = &model.coefficients;
    let mut i = a;
    let mut x = b;
    let mut y = c;
    let mut acc = a.wrapping_mul(k[0])
        .wrapping_add(b.wrapping_mul(k[1]))
        .wrapping_sub(c);
    while i > 0 {
        if compare(model.conditions[0], i, x) {
            acc = acc.wrapping_add(i.wrapping_mul(k[2])).wrapping_sub(y);
            x = x.wrapping_add(k[3]);
        } else if compare(model.conditions[1], x, y) {
            acc = acc.wrapping_sub(i.wrapping_mul(k[4])).wrapping_add(x);
            y = y.wrapping_sub(k[5]);
        } else {
            acc = acc.wrapping_add(x.wrapping_mul(k[6])).wrapping_sub(i);
            x = x.wrapping_sub(k[7]);
        }
        if compare(model.conditions[2], acc, y) {
            acc = acc.wrapping_sub(x.wrapping_mul(k[8]));
        } else {
            acc = acc.wrapping_add(y.wrapping_mul(k[9]));
        }
        i -= 1;
    }
    if compare(model.conditions[3], acc, 0) {
        acc.wrapping_add(c.wrapping_mul(k[10]))
    } else {
        acc.wrapping_sub(b.wrapping_mul(k[11]))
    }
}

fn literal(v: i32) -> String {
    if v == i32::MIN { "(0 - 2147483647 - 1)".into() }
    else if v < 0 {format!("({v})")} else {v.to_string()}
}

fn procedure(model: &Model) -> String {
    let k = &model.coefficients;
    let c = &model.conditions;
    format!(
        "  PROCEDURE Probe*(a, b, c: INTEGER): INTEGER;\n\
         \x20   VAR i, x, y, acc: INTEGER;\n\
         \x20 BEGIN\n\
         \x20   i := a; x := b; y := c;\n\
         \x20   acc := a * {k0} + b * {k1} - c;\n\
         \x20   WHILE i > 0 DO\n\
         \x20     IF i {c0} x THEN\n\
         \x20       acc := acc + i * {k2} - y;\n\
         \x20       x := x + {k3}\n\
         \x20     ELSIF x {c1} y THEN\n\
         \x20       acc := acc - i * {k4} + x;\n\
         \x20       y := y - {k5}\n\
         \x20     ELSE\n\
         \x20       acc := acc + x * {k6} - i;\n\
         \x20       x := x - {k7}\n\
         \x20     END;\n\
         \x20     IF acc {c2} y THEN\n\
         \x20       acc := acc - x * {k8}\n\
         \x20     ELSE\n\
         \x20       acc := acc + y * {k9}\n\
         \x20     END;\n\
         \x20     i := i - 1\n\
         \x20   END;\n\
         \x20   IF acc {c3} 0 THEN\n\
         \x20     acc := acc + c * {k10}\n\
         \x20   ELSE\n\
         \x20     acc := acc - b * {k11}\n\
         \x20   END;\n\
         \x20   RETURN acc\n\
         \x20 END Probe;\n",
        k0=k[0],k1=k[1],k2=k[2],k3=k[3],k4=k[4],k5=k[5],
        k6=k[6],k7=k[7],k8=k[8],k9=k[9],k10=k[10],k11=k[11],
        c0=c[0],c1=c[1],c2=c[2],c3=c[3],
    )
}

fn inputs(seed: u64) -> Vec<(i32, i32, i32)> {
    let mut rng = Rng::new(seed ^ 0xa42e_8190_9dad_70f3);
    let mut cases = vec![
        (-7, 0, 0), (0, 0, 0), (1, 0, 0),
        (2, 1, -1), (5, -1, 1), (9, 9, 9),
        (12, i32::MIN, i32::MAX),
        (12, i32::MAX, i32::MIN),
        (4, i32::MIN, 0),
        (8, 0, i32::MAX),
        (7, -7, -7), (10, 2147483647, 2147483647),
    ];
    for _ in 0..32 {
        let a = rng.range(-5, 14);
        let b = if rng.below(3) == 0 {
            rng.range(-20, 20)
        } else {rng.next() as i32};
        let c = if rng.below(3) == 0 {
            rng.range(-20, 20)
        } else {rng.next() as i32};
        cases.push((a,b,c));
    }
    cases
}

fn module(model: &Model, cases: &[(i32,i32,i32)],
          include_checks: bool) -> String
{
    let mut text = String::from("MODULE Fuzz;\n");
    text.push_str(&procedure(model));
    if include_checks {
        text.push_str("BEGIN\n");
        for &(a,b,c) in cases {
            let want = expected(model,a,b,c);
            writeln!(text, "  IF Probe({}, {}, {}) # {} THEN",
                literal(a),literal(b),literal(c),literal(want)).unwrap();
            text.push_str("    ASSERT(FALSE)\n  END;\n");
        }
    }
    text.push_str("END Fuzz.\n");
    text
}

fn harness(model: &Model, cases: &[(i32,i32,i32)], seed: u64) -> String {
    let mut text=String::from(
        "#include <stdint.h>\n#include <stdio.h>\n\
         extern int32_t Fuzz__Probe(int32_t,int32_t,int32_t);\n\
         int main(void) {\n");
    for (index,&(a,b,c)) in cases.iter().enumerate() {
        let result=expected(model,a,b,c);
        writeln!(text,
            "  if (Fuzz__Probe({}, {}, {}) != {}) {{\n    fprintf(stderr, \"seed {} case {} mismatch\\n\"); return 1;\n  }}",
            literal(a),literal(b),literal(c),literal(result),seed,index).unwrap();
    }
    writeln!(text, "  puts(\"PASS compiler-subset seed {seed}: {} cases\");",
        cases.len()).unwrap();
    text.push_str("  return 0;\n}\n");
    text
}

/// --compiler-subset=source emits input for the standalone compiler.
/// --compiler-subset=selfcheck emits the *same function* plus independent
/// runtime assertions for the existing Oberon C and Cranelift backends.
/// --compiler-subset=harness emits the C oracle for the standalone assembly.
pub(super) fn render(seed:u64,mode:&str)->Result<String,String> {
    let model=make(seed);
    let cases=inputs(seed);
    match mode {
        "source"=>Ok(module(&model,&cases,false)),
        "selfcheck"=>Ok(module(&model,&cases,true)),
        "harness"=>Ok(harness(&model,&cases,seed)),
        _=>Err(format!(
            "unknown compiler subset mode {mode}; use source, selfcheck or harness")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_sources_share_identical_procedures() {
        for seed in [1,7,17,1075,1098] {
            let source=render(seed,"source").unwrap();
            let checking=render(seed,"selfcheck").unwrap();
            assert!(source.starts_with("MODULE Fuzz;"));
            assert!(checking.starts_with(source.trim_end_matches("END Fuzz.\n")));
            assert_eq!(checking.matches("ASSERT(FALSE)").count(),44);
            let harness=render(seed,"harness").unwrap();
            assert_eq!(harness.matches("Fuzz__Probe(").count(),45);
            assert!(source.contains("WHILE i > 0 DO"));
            assert!(source.contains("ELSIF"));
            assert_eq!(source,render(seed,"source").unwrap());
        }
    }
    #[test]
    fn model_handles_overflow_and_nonpositive_iteration_limits() {
        for seed in [0,1,7,17,1000] {
            let model=make(seed);
            for (a,b,c) in inputs(seed) {
                let result=expected(&model,a,b,c);
                if a<=0 {
                    let k=&model.coefficients;
                    let initial=a.wrapping_mul(k[0])
                        .wrapping_add(b.wrapping_mul(k[1])).wrapping_sub(c);
                    let reference=if compare(model.conditions[3],initial,0) {
                        initial.wrapping_add(c.wrapping_mul(k[10]))
                    } else {
                        initial.wrapping_sub(b.wrapping_mul(k[11]))
                    };
                    assert_eq!(result,reference);
                }
            }
        }
    }
}
