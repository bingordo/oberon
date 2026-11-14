//! Seeded language-feature stress cases for the standalone native compiler.
//! A compact parameterized program shape covers interactions missing from the
//! historical scalar/array generators; a separate Rust evaluator supplies
//! all expected results. Only checked, terminating, defined inputs are used.
use super::Rng;
use std::fmt::Write as _;

const INPUTS:usize=44;

#[derive(Clone,Debug)]
struct RichCase {
    rows:usize,
    cols:usize,
    row:usize,
    col:usize,
    iterations:i32,
    step:i32,
    initial:i32,
    divisor_mod:i32,
}

impl RichCase {
    /// Select nontrivial dimensions, a bounded loop and divisor choices from the seed.
    fn generate(seed:u64)->Self {
        let mut r=Rng::new(seed^0xb001_ea57_9d14_62c3);
        let rows=2+r.below(3);
        let cols=2+r.below(4);
        Self {
            rows,cols,
            row:r.below(rows),
            col:r.below(cols),
            iterations:1+r.below(4) as i32,
            step:1+r.below(6) as i32,
            initial:r.range(-30,30),
            divisor_mod:3+r.below(8) as i32,
        }
    }

    /// Divide toward negative infinity even for a negative divisor.
    fn floor_div(a:i32,b:i32)->(i32,i32) {
        assert_ne!(b,0);
        let mut quotient=i64::from(a)/i64::from(b);
        let mut remainder=i64::from(a)%i64::from(b);
        if remainder!=0 && (remainder<0)!=(b<0) {
            quotient-=1;
            remainder+=i64::from(b);
        }
        (quotient as i32,remainder as i32)
    }

    /// Independently interpret the generated program, including skipped calls.
    fn expected(&self,a:i32,b:i32,c:i32)->i32 {
        // The generated divisor is never 0 or -1, including at i32::MIN.
        let mut divisor=b.rem_euclid(self.divisor_mod)+2;
        if c<0 {divisor=-divisor;}
        let (quotient,remainder)=Self::floor_div(a,divisor);
        let mut hits=0i32;

        // (a=0) OR Flag(a,a>0); Flag uses its own short-circuit AND.
        let p=if a==0 {
            true
        } else if a>0 {
            hits+=1; // Flag(TRUE) calls Touch exactly once
            true
        } else {
            false
        };
        let qflag=!p || c<0;

        // arr[Touch(row),Touch(col)] := quotient+remainder;
        // INC(arr[Touch(row),Touch(col)],c);
        // DEC(arr[Touch(row),Touch(col)],remainder);
        hits+=6; // six indices, never duplicated by either update
        let element=quotient.wrapping_add(remainder)
            .wrapping_add(c).wrapping_sub(remainder)
            .wrapping_add(1).wrapping_sub(self.step);
        let mut acc=self.initial;
        for _ in 0..self.iterations {
            acc=acc.wrapping_add(element).wrapping_sub(quotient);
        }
        // p OR (Touch(c)>0)
        if p || {hits+=1;c>0} {
            acc=acc.wrapping_add(1);
        } else {
            acc=acc.wrapping_sub(1);
        }
        // c=0 OR (17 DIV c>0) -- never evaluate the divide for c=0.
        if c==0 || Self::floor_div(17,c).0>0 {
            acc=acc.wrapping_add(2);
        } else {
            acc=acc.wrapping_sub(2);
        }
        if qflag && quotient>0 {
            acc=acc.wrapping_add(3);
        } else {
            acc=acc.wrapping_sub(3);
        }
        let choice=if p {c} else {c.wrapping_neg()};
        acc.wrapping_add(quotient).wrapping_add(remainder)
            .wrapping_add(hits.wrapping_mul(3)).wrapping_add(choice)
    }

    /// Render real Oberon procedures and a feature-rich exported Probe.
    fn procedures(&self)->String {
        format!(r#"MODULE Fuzz;
VAR hits: INTEGER;
  PROCEDURE Touch*(x: INTEGER): INTEGER;
  BEGIN hits := hits + 1; RETURN x END Touch;
  PROCEDURE Flag*(x: INTEGER; enabled: BOOLEAN): BOOLEAN;
  BEGIN RETURN enabled & (Touch(x) > 0) END Flag;
  PROCEDURE Choose*(enabled: BOOLEAN; x: INTEGER): INTEGER;
  VAR result: INTEGER;
  BEGIN
    result := x;
    IF ~enabled THEN result := -result END;
    RETURN result
  END Choose;
  PROCEDURE Probe*(a, b, c: INTEGER): INTEGER;
  VAR arr: ARRAY {rows}, {cols} OF INTEGER;
      i, j, k, divisor, quotient, remainder, acc: INTEGER;
      p, qflag: BOOLEAN;
  BEGIN
    hits := 0;
    i := {row}; j := {col};
    divisor := (b MOD {divisor_mod}) + 2;
    IF c < 0 THEN divisor := -divisor END;
    quotient := a DIV divisor;
    remainder := a MOD divisor;
    p := (a = 0) OR Flag(a, a > 0);
    qflag := ~p OR (c < 0);
    arr[Touch(i), Touch(j)] := quotient + remainder;
    INC(arr[Touch(i), Touch(j)], c);
    DEC(arr[Touch(i), Touch(j)], remainder);
    INC(arr[i,j]);
    DEC(arr[i,j], {step});
    acc := {initial};
    k := 0;
    REPEAT
      INC(acc, arr[i,j]);
      DEC(acc, quotient);
      INC(k)
    UNTIL k >= {iterations};
    IF p OR (Touch(c) > 0) THEN INC(acc) ELSE DEC(acc) END;
    IF (c = 0) OR (17 DIV c > 0) THEN INC(acc, 2) ELSE DEC(acc, 2) END;
    IF qflag & (quotient > 0) THEN INC(acc, 3) ELSE DEC(acc, 3) END;
    RETURN acc + quotient + remainder + hits * 3 + Choose(p, c)
  END Probe;
"#,rows=self.rows,cols=self.cols,row=self.row,col=self.col,
        divisor_mod=self.divisor_mod,step=self.step,initial=self.initial,
        iterations=self.iterations)
    }
}

/// Render a signed INTEGER literal without overflowing the Oberon lexer.
fn lit(v:i32)->String {
    if v==i32::MIN {"(0 - 2147483647 - 1)".into()}
    else if v<0 {format!("({v})")} else {v.to_string()}
}

/// Include signed extremes, zero guards, negative divisors and random inputs.
fn inputs(seed:u64)->Vec<(i32,i32,i32)> {
    let mut r=Rng::new(seed^0x942c_623a_1113_7e2b);
    let mut cases=vec![
        (0,0,0),(1,0,0),(-1,0,0),(0,1,-1),
        (0,-1,1),(7,-3,1),(-7,3,-1),
        (i32::MIN,i32::MAX,0),(i32::MAX,i32::MIN,0),
        (i32::MIN,-1,-1),(i32::MIN,i32::MIN,i32::MIN),
        (i32::MAX,i32::MAX,i32::MAX),
    ];
    while cases.len()<INPUTS {
        let value=|r:&mut Rng|->i32 {
            if r.below(3)==0 {r.range(-200,200)}
            else {r.next() as i32}
        };
        cases.push((value(&mut r),value(&mut r),value(&mut r)));
    }
    cases
}

/// Emit the identical source procedure in native and self-check variants.
fn module(program:&RichCase,cases:&[(i32,i32,i32)],checks:bool)->String {
    let mut out=program.procedures();
    if checks {
        out.push_str("BEGIN\n");
        for &(a,b,c) in cases {
            writeln!(out,"  IF Probe({}, {}, {}) # {} THEN ASSERT(FALSE) END;",
                lit(a),lit(b),lit(c),lit(program.expected(a,b,c))).unwrap();
        }
    }
    out.push_str("END Fuzz.\n");
    out
}

/// Emit a standalone C caller whose expected values come only from Rust.
fn harness(program:&RichCase,cases:&[(i32,i32,i32)],seed:u64)->String {
    let mut out=String::from(
        "#include <stdint.h>\n#include <stdio.h>\n\
         extern int32_t Fuzz__Probe(int32_t,int32_t,int32_t);\n\
         int main(void) {\n");
    for (index,&(a,b,c)) in cases.iter().enumerate() {
        writeln!(out,
            "  if (Fuzz__Probe({}, {}, {}) != {}) {{\n\
             \x20  fprintf(stderr, \"rich seed {seed} case {index} mismatch\\n\"); return 1;\n\
             \x20}}",
            lit(a),lit(b),lit(c),lit(program.expected(a,b,c))).unwrap();
    }
    writeln!(out,"  puts(\"PASS rich seed {seed}: {} cases\");",cases.len()).unwrap();
    out.push_str("  return 0;\n}\n");
    out
}

/// Dispatch one of the three synchronized representations of a rich case.
pub(super) fn render(seed:u64,mode:&str)->Result<String,String> {
    let program=RichCase::generate(seed);
    let cases=inputs(seed);
    match mode {
        "rich-source"=>Ok(module(&program,&cases,false)),
        "rich-selfcheck"=>Ok(module(&program,&cases,true)),
        "rich-harness"=>Ok(harness(&program,&cases,seed)),
        _=>Err(format!("unknown rich compiler subset mode {mode}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rich_generator_is_reproducible_and_self_oracled() {
        for seed in [0,1,2,3,7,17,1000,9999] {
            let src=render(seed,"rich-source").unwrap();
            let checks=render(seed,"rich-selfcheck").unwrap();
            let c=render(seed,"rich-harness").unwrap();
            assert_eq!(src,render(seed,"rich-source").unwrap());
            assert!(checks.starts_with(src.trim_end_matches("END Fuzz.\n")));
            assert_eq!(checks.matches("ASSERT(FALSE)").count(),INPUTS);
            assert_eq!(c.matches("Fuzz__Probe(").count(),INPUTS+1);
            assert_eq!(inputs(seed).len(),INPUTS);
        }
    }
    #[test]
    fn rich_programs_have_required_feature_witnesses() {
        for seed in 0..32 {
            let src=render(seed,"rich-source").unwrap();
            for witness in [" DIV "," MOD ","INC(","DEC(","REPEAT",
                "UNTIL","BOOLEAN"," OR "," & ","~p",
                "Flag(a, a > 0)","Choose(p, c)","Touch(i), Touch(j)"] {
                assert!(src.contains(witness),"seed {seed} misses {witness}");
            }
            assert!(src.contains("ARRAY 2,") || src.contains("ARRAY 3,")
                || src.contains("ARRAY 4,"));
        }
        let cases=(0..64).map(|seed|RichCase::generate(seed)).collect::<Vec<_>>();
        assert!(cases.windows(2).any(|w|w[0].rows!=w[1].rows));
        assert!(cases.windows(2).any(|w|w[0].iterations!=w[1].iterations));
        assert!(cases.windows(2).any(|w|w[0].step!=w[1].step));
    }
    #[test]
    fn oracle_signed_floor_div_and_boundaries() {
        assert_eq!(RichCase::floor_div(-7,3),(-3,2));
        assert_eq!(RichCase::floor_div(7,-3),(-3,-2));
        assert_eq!(RichCase::floor_div(-7,-3),(2,-1));
        assert_eq!(RichCase::floor_div(i32::MIN,2),(-1073741824,0));
        for seed in [0,1,2,7,99] {
            let program=RichCase::generate(seed);
            for (a,b,c) in inputs(seed) {
                assert_eq!(program.expected(a,b,c),
                    RichCase::generate(seed).expected(a,b,c));
            }
        }
    }
}
