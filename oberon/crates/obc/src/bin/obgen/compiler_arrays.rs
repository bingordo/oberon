//! Deterministic checked-array cases for the standalone compiler's
//! three-backend differential suite. Source, self-check and C harness are
//! rendered from one immutable scenario; the expected values are computed
//! by this independent Rust interpreter, never by a compiled function.
use super::Rng;
use std::fmt::Write as _;

const INPUTS:usize=44;
const LENGTHS:[usize;6]=[2,3,5,8,17,200];

fn lit(n:i32)->String {
    if n==i32::MIN {"(0 - 2147483647 - 1)".into()}
    else if n<0 {format!("({n})")} else {n.to_string()}
}

#[derive(Clone,Debug)]
struct ArrayCase {
    len:usize,
    offset:i32,
    multiplier:i32,
    stride:i32,
    bias:i32,
}
impl ArrayCase {
    fn generate(seed:u64)->Self {
        let mut r=Rng::new(seed^0xaf71_520b_94d3_a291);
        Self {
            len:LENGTHS[r.below(LENGTHS.len())],
            offset:r.range(-23,23),
            multiplier:r.range(-7,7),
            stride:r.range(-5,5),
            bias:r.range(-9,9),
        }
    }
    fn expected(&self,a:i32,b:i32,c:i32)->i32 {
        let mut array=vec![0i32;self.len];
        let mut x=a.wrapping_add(self.offset);
        for (i,cell) in array.iter_mut().enumerate() {
            let value=x.wrapping_add((i as i32).wrapping_mul(self.multiplier))
                .wrapping_add(b);
            *cell=if value>c {value.wrapping_sub(c)}
                  else {value.wrapping_add(c)};
            x=x.wrapping_add(self.stride);
        }
        let mut acc=0i32;
        for value in &array {
            acc=if *value>=0 {acc.wrapping_add(*value)}
                else {acc.wrapping_sub(*value)};
        }
        array[0]=array[0].wrapping_add(a);
        let last=self.len-1;
        array[last]=array[last].wrapping_sub(b);
        acc.wrapping_add(array[0]).wrapping_sub(array[last])
            .wrapping_add(self.bias)
    }
    fn procedure(&self)->String {
        // Every read is preceded by a store. Both loops use signed indices
        // constrained to 0..N-1. Explicit accesses to element zero and the
        // last element stress both boundary addressing modes.
        format!("  PROCEDURE Probe*(a, b, c: INTEGER): INTEGER;\n\
                 \x20   VAR arr: ARRAY {} OF INTEGER;\n\
                 \x20       i, x, acc: INTEGER;\n\
                 \x20 BEGIN\n\
                 \x20   i := 0; x := a + {}; acc := 0;\n\
                 \x20   WHILE i < {} DO\n\
                 \x20     arr[i] := x + i * {} + b;\n\
                 \x20     IF arr[i] > c THEN\n\
                 \x20       arr[i] := arr[i] - c\n\
                 \x20     ELSE\n\
                 \x20       arr[i] := arr[i] + c\n\
                 \x20     END;\n\
                 \x20     x := x + {};\n\
                 \x20     i := i + 1\n\
                 \x20   END;\n\
                 \x20   i := 0;\n\
                 \x20   WHILE i < {} DO\n\
                 \x20     IF arr[i] >= 0 THEN\n\
                 \x20       acc := acc + arr[i]\n\
                 \x20     ELSE\n\
                 \x20       acc := acc - arr[i]\n\
                 \x20     END;\n\
                 \x20     i := i + 1\n\
                 \x20   END;\n\
                 \x20   arr[0] := arr[0] + a;\n\
                 \x20   arr[{}] := arr[{}] - b;\n\
                 \x20   RETURN acc + arr[0] - arr[{}] + {}\n\
                 \x20 END Probe;\n",
            self.len,lit(self.offset),self.len,lit(self.multiplier),
            lit(self.stride),self.len,self.len-1,self.len-1,
            self.len-1,lit(self.bias))
    }
}
fn inputs(seed:u64)->Vec<(i32,i32,i32)> {
    let mut r=Rng::new(seed^0x55a6_2184_bc91_7213);
    let mut cases=vec![
        (0,0,0),(1,0,0),(-1,0,0),(0,1,-1),
        (0,-1,1),(2,3,5),(8,-7,17),(-8,7,-17),
        (i32::MIN,i32::MAX,0),(i32::MAX,i32::MIN,0),
        (i32::MIN,i32::MIN,i32::MAX),
        (i32::MAX,i32::MAX,i32::MIN),
    ];
    for _ in cases.len()..INPUTS {
        let x=|r:&mut Rng| ->i32 {
            if r.below(3)==0 {r.range(-100,100)}
            else {r.next() as i32}
        };
        cases.push((x(&mut r),x(&mut r),x(&mut r)));
    }
    cases
}
fn module(program:&ArrayCase,inputs:&[(i32,i32,i32)],checks:bool)->String {
    let mut out=String::from("MODULE Fuzz;\n");
    out.push_str(&program.procedure());
    if checks {
        out.push_str("BEGIN\n");
        for &(a,b,c) in inputs {
            writeln!(out,"  IF Probe({}, {}, {}) # {} THEN",
                lit(a),lit(b),lit(c),lit(program.expected(a,b,c))).unwrap();
            out.push_str("    ASSERT(FALSE)\n  END;\n");
        }
    }
    out.push_str("END Fuzz.\n");
    out
}
fn harness(program:&ArrayCase,inputs:&[(i32,i32,i32)],seed:u64)->String {
    let mut out=String::from(
        "#include <stdint.h>\n#include <stdio.h>\n\
         extern int32_t Fuzz__Probe(int32_t,int32_t,int32_t);\n\
         int main(void) {\n");
    for (index,&(a,b,c)) in inputs.iter().enumerate() {
        writeln!(out,
            "  if (Fuzz__Probe({}, {}, {}) != {}) {{\n\
             \x20   fprintf(stderr, \"array seed {seed} case {index} mismatch\\n\"); return 1;\n\
             \x20 }}",
            lit(a),lit(b),lit(c),lit(program.expected(a,b,c))).unwrap();
    }
    writeln!(out,"  puts(\"PASS array seed {seed}: {} cases\");",inputs.len()).unwrap();
    out.push_str("  return 0;\n}\n");
    out
}
pub(super) fn render(seed:u64,mode:&str)->Result<String,String> {
    let program=ArrayCase::generate(seed);
    let cases=inputs(seed);
    match mode {
        "array-source"=>Ok(module(&program,&cases,false)),
        "array-selfcheck"=>Ok(module(&program,&cases,true)),
        "array-harness"=>Ok(harness(&program,&cases,seed)),
        _=>Err(format!("unsupported array compiler subset mode: {mode}")),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arrays_are_deterministic_and_self_oracled(){
        for seed in [1,2,3,5,11,17,107,9999] {
            let source=render(seed,"array-source").unwrap();
            let checks=render(seed,"array-selfcheck").unwrap();
            let harness=render(seed,"array-harness").unwrap();
            assert_eq!(source,render(seed,"array-source").unwrap());
            assert!(source.contains("ARRAY "));
            assert!(source.contains("arr[0]"));
            assert!(source.contains("WHILE i <"));
            assert_eq!(checks.matches("ASSERT(FALSE)").count(),INPUTS);
            assert_eq!(harness.matches("Fuzz__Probe(").count(),INPUTS+1);
            assert!(checks.starts_with(source.trim_end_matches("END Fuzz.\n")));
            assert_eq!(inputs(seed).len(),INPUTS);
        }
    }
    #[test]
    fn seeds_cover_short_and_large_array_layouts(){
        let lengths:(Vec<usize>)=(1..100).map(|seed|
            ArrayCase::generate(seed).len).collect();
        for length in LENGTHS {assert!(lengths.contains(&length));}
        let varied:(Vec<_>)=(1..12).map(|seed|
            ArrayCase::generate(seed).expected(7,-3,15)).collect();
        assert!(varied.windows(2).any(|w|w[0]!=w[1]));
    }
}
