//! Seeded, self-oracled programs for the small standalone compiler.
//! The old full Oberon fuzzer remains unchanged. This mode reuses its PRNG,
//! but creates randomized typed expression trees and structured CFGs.
//! The independent interpreter never consults a compiler's output.
use super::Rng;
use std::fmt::Write as _;

const NAMES:[&str;8]=["a","b","c","i","x","y","acc","j"];
const COMPARISONS:[&str;6]=["=","#","<","<=",">",">="];

#[derive(Clone,Copy,Debug)]
enum Bin {Add,Sub,Mul}
impl Bin {
    fn spelling(self)->&'static str {
        match self {Self::Add=>"+",Self::Sub=>"-",Self::Mul=>"*"}
    }
    fn apply(self,a:i32,b:i32)->i32 {
        match self {
            Self::Add=>a.wrapping_add(b),
            Self::Sub=>a.wrapping_sub(b),
            Self::Mul=>a.wrapping_mul(b),
        }
    }
}
#[derive(Clone,Debug)]
enum Expr {
    Var(usize),Lit(i32),Binary(Bin,Box<Expr>,Box<Expr>),
}
impl Expr {
    fn evaluate(&self,vars:&[i32;8])->i32 {
        match self {
            Self::Var(i)=>vars[*i],
            Self::Lit(n)=>*n,
            Self::Binary(op,a,b)=>op.apply(a.evaluate(vars),b.evaluate(vars)),
        }
    }
    fn render(&self)->String {
        match self {
            Self::Var(i)=>NAMES[*i].to_string(),
            Self::Lit(n)=>literal(*n),
            Self::Binary(op,a,b)=>format!(
                "({} {} {})",a.render(),op.spelling(),b.render()),
        }
    }
}
#[derive(Clone,Debug)]
struct Condition {lhs:Expr,op:&'static str,rhs:Expr}
impl Condition {
    fn evaluate(&self,vars:&[i32;8])->bool {
        let a=self.lhs.evaluate(vars);
        let b=self.rhs.evaluate(vars);
        match self.op {
            "="=>a==b,"#"=>a!=b,"<"=>a<b,"<="=>a<=b,
            ">"=>a>b,">="=>a>=b,_=>unreachable!(),
        }
    }
    fn render(&self)->String {
        format!("{} {} {}",self.lhs.render(),self.op,self.rhs.render())
    }
}
#[derive(Clone,Debug)]
enum Statement {
    Assign(usize,Expr),
    If {arms:Vec<(Condition,Vec<Statement>)>,otherwise:Vec<Statement>},
    While {guard:Condition,body:Vec<Statement>},
}
impl Statement {
    fn interpret(&self,vars:&mut [i32;8],fuel:&mut usize) {
        assert!(*fuel>0,"generated loop did not terminate");
        *fuel-=1;
        match self {
            Self::Assign(dst,expr)=>vars[*dst]=expr.evaluate(vars),
            Self::If{arms,otherwise}=>{
                let statements=arms.iter().find(|(cond,_)|cond.evaluate(vars))
                    .map_or(otherwise,|(_,body)|body);
                for statement in statements {
                    statement.interpret(vars,fuel);
                }
            }
            Self::While{guard,body}=>{
                while guard.evaluate(vars) {
                    for statement in body { statement.interpret(vars,fuel); }
                }
            }
        }
    }
    fn render(&self,out:&mut String,indent:usize) {
        let pad=" ".repeat(indent);
        match self {
            Self::Assign(dst,expr)=>{
                writeln!(out,"{pad}{} := {}",NAMES[*dst],expr.render()).unwrap();
            }
            Self::If{arms,otherwise}=>{
                for (i,(condition,body)) in arms.iter().enumerate() {
                    let keyword=if i==0 {"IF"} else {"ELSIF"};
                    writeln!(out,"{pad}{keyword} {} THEN",condition.render()).unwrap();
                    render_sequence(out,body,indent+2);
                }
                if !otherwise.is_empty() {
                    writeln!(out,"{pad}ELSE").unwrap();
                    render_sequence(out,otherwise,indent+2);
                }
                writeln!(out,"{pad}END").unwrap();
            }
            Self::While{guard,body}=>{
                writeln!(out,"{pad}WHILE {} DO",guard.render()).unwrap();
                render_sequence(out,body,indent+2);
                writeln!(out,"{pad}END").unwrap();
            }
        }
    }
}
fn render_sequence(out:&mut String,statements:&[Statement],indent:usize) {
    for (i,statement) in statements.iter().enumerate() {
        statement.render(out,indent);
        if i+1<statements.len() {out.push_str("  ;\n");}
    }
}
fn literal(v:i32)->String {
    if v==i32::MIN {"(0 - 2147483647 - 1)".to_string()}
    else if v<0 {format!("({v})")} else {v.to_string()}
}
fn v(i:usize)->Expr {Expr::Var(i)}
fn lit(i:i32)->Expr {Expr::Lit(i)}
fn bin(op:Bin,a:Expr,b:Expr)->Expr {Expr::Binary(op,Box::new(a),Box::new(b))}
fn assign(i:usize,e:Expr)->Statement {Statement::Assign(i,e)}
fn compare(a:Expr,op:&'static str,b:Expr)->Condition {
    Condition{lhs:a,op,rhs:b}
}
fn expression(r:&mut Rng,depth:usize)->Expr {
    if depth==0 || r.below(4)==0 {
        return if r.below(3)==0 {
            lit(*r.pick(&[-7,-1,0,1,2,3,7,17,i32::MAX,i32::MIN]))
        } else {
            v(r.below(NAMES.len()))
        };
    }
    let op=match r.below(3){0=>Bin::Add,1=>Bin::Sub,_=>Bin::Mul};
    bin(op,expression(r,depth-1),expression(r,depth-1))
}
fn condition(r:&mut Rng)->Condition {
    let op=COMPARISONS[r.below(COMPARISONS.len())];
    compare(expression(r,1),op,expression(r,1))
}
fn branch(r:&mut Rng,depth:usize)->Statement {
    let branches=1+r.below(2);
    let mut arms=Vec::new();
    for _ in 0..branches {
        let count=1+r.below(2);
        arms.push((condition(r),(0..count)
            .map(|_|statement(r,depth)).collect()));
    }
    let otherwise=if r.below(4)==0 {Vec::new()} else {
        (0..1+r.below(2)).map(|_|statement(r,depth)).collect()
    };
    Statement::If{arms,otherwise}
}
fn statement(r:&mut Rng,depth:usize)->Statement {
    if depth>0 && r.below(3)==0 {return branch(r,depth-1);}
    // Avoid touching either controlled loop induction variable i or j.
    let target=match r.below(3){0=>4,1=>5,_=>6};
    assign(target,expression(r,2))
}

#[derive(Clone,Debug)]
struct Program {body:Vec<Statement>}
impl Program {
    fn generate(seed:u64)->Self {
        let mut r=Rng::new(seed);
        let mut body=vec![
            assign(3,v(0)),assign(4,v(1)),assign(5,v(2)),
            assign(6,bin(Bin::Sub,bin(Bin::Add,v(0),v(1)),v(2))),
            assign(7,lit(0)),
        ];
        // A seed chooses zero/one/two outer while loops, with different
        // branches and arithmetic generated independently for each body.
        let outer_count=r.below(3);
        for _ in 0..outer_count {
            let mut loop_body=Vec::new();
            for _ in 0..1+r.below(3) {
                loop_body.push(statement(&mut r,2));
            }
            if r.below(2)==0 {
                // Optional bounded nested loop. The induction variable j
                // is never assigned by randomly generated statements.
                let bound=r.range(1,3);
                loop_body.push(assign(7,lit(0)));
                let mut inner=vec![statement(&mut r,1)];
                inner.push(assign(7,bin(Bin::Add,v(7),lit(1))));
                loop_body.push(Statement::While{
                    guard:compare(v(7),"<",lit(bound)),body:inner,
                });
            }
            // Exactly one decrement of i: all generated loops terminate,
            // including when arithmetic in other locals wraps.
            loop_body.push(assign(3,bin(Bin::Sub,v(3),lit(1))));
            body.push(Statement::While{
                guard:compare(v(3),">",lit(0)),body:loop_body,
            });
            // Optionally restore a bounded loop count for another loop.
            if r.below(2)==0 {
                body.push(assign(3,bin(Bin::Sub,v(0),lit(r.range(0,5)))));
            }
        }
        // Always include post-loop diamonds; further nesting is optional.
        for _ in 0..1+r.below(3) {
            body.push(branch(&mut r,2));
        }
        // Finish with an independent expression tree containing live locals.
        body.push(assign(6,bin(Bin::Add,v(6),expression(&mut r,2))));
        Self{body}
    }
    fn expected(&self,a:i32,b:i32,c:i32)->i32 {
        let mut vars=[a,b,c,0,0,0,0,0];
        let mut fuel=8192;
        for stmt in &self.body {stmt.interpret(&mut vars,&mut fuel);}
        vars[6]
    }
    fn render_procedure(&self)->String {
        let mut source=String::from(
            "  PROCEDURE Probe*(a, b, c: INTEGER): INTEGER;\n\
             \x20   VAR i, x, y, acc, j: INTEGER;\n\
             \x20 BEGIN\n");
        render_sequence(&mut source,&self.body,4);
        source.push_str("  ;\n    RETURN acc\n  END Probe;\n");
        source
    }
}
fn inputs(seed:u64)->Vec<(i32,i32,i32)> {
    let mut r=Rng::new(seed^0xa42e_8190_9dad_70f3);
    let mut cases=vec![
        (-7,0,0),(0,0,0),(1,0,0),(2,1,-1),(5,-1,1),
        (9,9,9),(12,i32::MIN,i32::MAX),(12,i32::MAX,i32::MIN),
        (4,i32::MIN,0),(8,0,i32::MAX),(7,-7,-7),
        (10,i32::MAX,i32::MAX),
    ];
    for _ in 0..32 {
        let a=r.range(-5,14);
        let b=if r.below(3)==0 {r.range(-20,20)} else {r.next() as i32};
        let c=if r.below(3)==0 {r.range(-20,20)} else {r.next() as i32};
        cases.push((a,b,c));
    }
    cases
}
fn module(program:&Program,cases:&[(i32,i32,i32)],checks:bool)->String {
    let mut source=String::from("MODULE Fuzz;\n");
    source.push_str(&program.render_procedure());
    if checks {
        source.push_str("BEGIN\n");
        for &(a,b,c) in cases {
            writeln!(source,"  IF Probe({}, {}, {}) # {} THEN",
                literal(a),literal(b),literal(c),
                literal(program.expected(a,b,c))).unwrap();
            source.push_str("    ASSERT(FALSE)\n  END;\n");
        }
    }
    source.push_str("END Fuzz.\n");
    source
}
fn harness(program:&Program,cases:&[(i32,i32,i32)],seed:u64)->String {
    let mut text=String::from(
        "#include <stdint.h>\n#include <stdio.h>\n\
         extern int32_t Fuzz__Probe(int32_t,int32_t,int32_t);\n\
         int main(void) {\n");
    for (index,&(a,b,c)) in cases.iter().enumerate() {
        writeln!(text,
            "  if (Fuzz__Probe({}, {}, {}) != {}) {{\n    fprintf(stderr, \"seed {} case {} mismatch\\n\"); return 1;\n  }}",
            literal(a),literal(b),literal(c),literal(program.expected(a,b,c)),
            seed,index).unwrap();
    }
    writeln!(text,"  puts(\"PASS compiler-subset seed {seed}: {} cases\");",cases.len()).unwrap();
    text.push_str("  return 0;\n}\n");
    text
}
pub(super) fn render(seed:u64,mode:&str)->Result<String,String> {
    let program=Program::generate(seed);
    let cases=inputs(seed);
    match mode {
        "source"=>Ok(module(&program,&cases,false)),
        "selfcheck"=>Ok(module(&program,&cases,true)),
        "harness"=>Ok(harness(&program,&cases,seed)),
        _=>Err(format!("unknown compiler subset mode {mode}; use source, selfcheck or harness")),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_functions_identical_across_backends_and_repeatable() {
        for seed in [1,2,3,7,17,1075,1098,2112,9999] {
            let source=render(seed,"source").unwrap();
            let check=render(seed,"selfcheck").unwrap();
            assert!(check.starts_with(source.trim_end_matches("END Fuzz.\n")));
            assert_eq!(check.matches("ASSERT(FALSE)").count(),44);
            assert_eq!(render(seed,"source").unwrap(),source);
            assert_eq!(render(seed,"harness").unwrap().matches("Fuzz__Probe(").count(),45);
            assert!(source.contains("RETURN acc"));
        }
    }
    #[test]
    fn different_seeds_generate_different_cfgs_and_expressions() {
        let examples:(Vec<_>,Vec<_>)=((1..48).map(|seed|render(seed,"source").unwrap()).collect(),
                                       (1..48).map(|seed|Program::generate(seed)).collect());
        let sources=&examples.0;
        assert!(sources.iter().any(|s|s.contains("ELSIF")));
        assert!(sources.iter().any(|s|s.contains("WHILE i > 0")));
        assert!(sources.iter().any(|s|s.contains("WHILE j <")));
        assert!(sources.iter().any(|s|!s.contains("WHILE i > 0")));
        assert!(sources.iter().any(|s|s.matches("IF ").count()>3));
        assert!(sources.iter().any(|s|s.contains(" * ")));
        assert!(sources.iter().any(|s|s.contains(" + ")));
        assert!(sources.iter().any(|s|s.contains(" - ")));
        assert!(sources.windows(2).any(|w|w[0]!=w[1]));
    }
    #[test]
    fn repeatability_and_boundary_inputs() {
        for seed in [0,1,7,17,1000] {
            let p=Program::generate(seed);
            for (a,b,c) in inputs(seed) {
                assert_eq!(p.expected(a,b,c),Program::generate(seed).expected(a,b,c));
            }
        }
    }
}
