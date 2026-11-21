//! Type-directed, recursively generated Oberon differential programs.
//! No language interpreter or statically calculated expected outputs:
//! render the same typed, bounded AST as Oberon and as independent C,
//! compile both, and compare runtime results for reproducible inputs.
use super::Rng;
use std::fmt::Write as _;

const INPUTS:usize=32;
const INT_NAMES:[&str;6]=["a","b","c","x","y","acc"];

#[derive(Clone,Copy,Debug,PartialEq,Eq)]
enum Type {Integer,Boolean}
#[derive(Clone,Copy,Debug)]
enum IOp {Add,Sub,Mul,Div,Mod}
impl IOp {
    fn oberon(self)->&'static str {
        match self {Self::Add=>"+",Self::Sub=>"-",Self::Mul=>"*",
            Self::Div=>"DIV",Self::Mod=>"MOD"}
    }
    fn c(self)->&'static str {
        match self {Self::Add=>"+",Self::Sub=>"-",Self::Mul=>"*",
            Self::Div=>"floor_div",Self::Mod=>"floor_mod"}
    }
}
#[derive(Clone,Copy,Debug)]
enum Rel {Eq,Ne,Lt,Le,Gt,Ge}
impl Rel {
    fn oberon(self)->&'static str {
        match self {Self::Eq=>"=",Self::Ne=>"#",Self::Lt=>"<",
            Self::Le=>"<=",Self::Gt=>">",Self::Ge=>">="}
    }
    fn c(self)->&'static str {
        match self {Self::Eq=>"==",Self::Ne=>"!=",Self::Lt=>"<",
            Self::Le=>"<=",Self::Gt=>">",Self::Ge=>">="}
    }
    fn generate(r:&mut Rng)->Self {
        match r.below(6) {
            0=>Self::Eq,1=>Self::Ne,2=>Self::Lt,
            3=>Self::Le,4=>Self::Gt,_=>Self::Ge,
        }
    }
}
#[derive(Clone,Debug)]
enum Integer {
    Variable(usize),
    Literal(i32),
    /// All operands are pure. Effectful calls occur only in Boolean trees,
    /// where AND/OR enforce a language-defined evaluation order.
    Binary(IOp,Box<Integer>,Box<Integer>),
}
#[derive(Clone,Debug)]
enum Boolean {
    Literal(bool),
    Variable,
    Compare(Rel,Box<Integer>,Box<Integer>),
    /// The Touch call increments a private module counter. This permits
    /// verification of exact short-circuit side effects.
    TouchCompare(Rel,Box<Integer>,Box<Integer>),
    Not(Box<Boolean>),
    And(Box<Boolean>,Box<Boolean>),
    Or(Box<Boolean>,Box<Boolean>),
}
#[derive(Clone,Debug)]
enum TypedExpr {
    Integer(Integer),
    Boolean(Boolean),
}

fn literal(n:i32)->String {
    if n==i32::MIN {"(0 - 2147483647 - 1)".into()}
    else if n<0 {format!("({n})")} else {n.to_string()}
}
impl Integer {
    fn render(&self,c:bool)->String {
        match self {
            Self::Variable(i)=>INT_NAMES[*i].to_string(),
            Self::Literal(n)=>literal(*n),
            Self::Binary(op,a,b)=>{
                let (a,b)=(a.render(c),b.render(c));
                if c && matches!(op,IOp::Div|IOp::Mod) {
                    format!("{}({a}, {b})",op.c())
                } else {
                    format!("({a} {} {b})",if c {op.c()} else {op.oberon()})
                }
            }
        }
    }
    fn nodes(&self)->usize {
        match self {
            Self::Variable(_)|Self::Literal(_)=>1,
            Self::Binary(_,a,b)=>1+a.nodes()+b.nodes(),
        }
    }
}
impl Boolean {
    fn render(&self,c:bool)->String {
        match self {
            Self::Literal(v)=>if c {if *v {"1"} else {"0"}}
                else if *v {"TRUE"} else {"FALSE"}.to_string(),
            Self::Variable=>"flag".into(),
            Self::Compare(op,a,b)|Self::TouchCompare(op,a,b)=>{
                let lhs=if matches!(self,Self::TouchCompare(..)) {
                    format!("{}({})",if c {"touch"} else {"Touch"},a.render(c))
                } else {a.render(c)};
                format!("({lhs} {} {})",if c {op.c()} else {op.oberon()},b.render(c))
            }
            Self::Not(e)=>format!("({}{})",if c {"!"} else {"~"},e.render(c)),
            Self::And(a,b)=>format!("({} {} {})",a.render(c),if c {"&&"} else {"&"},b.render(c)),
            Self::Or(a,b)=>format!("({} {} {})",a.render(c),if c {"||"} else {"OR"},b.render(c)),
        }
    }
    fn nodes(&self)->usize {
        match self {
            Self::Literal(_)|Self::Variable=>1,
            Self::Compare(_,a,b)|Self::TouchCompare(_,a,b)=>1+a.nodes()+b.nodes(),
            Self::Not(x)=>1+x.nodes(),
            Self::And(a,b)|Self::Or(a,b)=>1+a.nodes()+b.nodes(),
        }
    }
}
impl TypedExpr {
    fn render(&self,c:bool)->String {
        match self {Self::Integer(e)=>e.render(c),Self::Boolean(e)=>e.render(c)}
    }
    fn ty(&self)->Type {
        match self {Self::Integer(_)=>Type::Integer,Self::Boolean(_)=>Type::Boolean}
    }
    fn nodes(&self)->usize {
        match self {Self::Integer(e)=>e.nodes(),Self::Boolean(e)=>e.nodes()}
    }
}
fn generate(r:&mut Rng,ty:Type,depth:usize)->TypedExpr {
    match ty {
        Type::Integer=>TypedExpr::Integer(integer(r,depth)),
        Type::Boolean=>TypedExpr::Boolean(boolean(r,depth)),
    }
}
fn integer(r:&mut Rng,depth:usize)->Integer {
    if depth==0 || r.below(5)==0 {
        return if r.below(4)==0 {
            Integer::Literal(*r.pick(&[-17,-7,-3,-1,0,1,2,3,7,19,i32::MIN,i32::MAX]))
        } else {Integer::Variable(r.below(INT_NAMES.len()))};
    }
    let op=match r.below(5) {
        0=>IOp::Add,1=>IOp::Sub,2=>IOp::Mul,3=>IOp::Div,_=>IOp::Mod,
    };
    let left=Box::new(integer(r,depth-1));
    // Never generate a zero divisor or the singular MIN/-1 quotient.
    // Non-unit signed constants still exercise Oberon's floor semantics.
    let right=if matches!(op,IOp::Div|IOp::Mod) {
        let d=2+r.below(7) as i32;
        Box::new(Integer::Literal(if r.below(2)==0 {d} else {-d}))
    } else {Box::new(integer(r,depth-1))};
    Integer::Binary(op,left,right)
}
fn boolean(r:&mut Rng,depth:usize)->Boolean {
    if depth==0 || r.below(6)==0 {
        return match r.below(5) {
            0=>Boolean::Literal(false),
            1=>Boolean::Literal(true),
            2=>Boolean::Variable,
            _=>Boolean::Compare(Rel::generate(r),
                Box::new(integer(r,1)),Box::new(integer(r,1))),
        };
    }
    match r.below(6) {
        0=>Boolean::Not(Box::new(boolean(r,depth-1))),
        1|2=>Boolean::And(Box::new(boolean(r,depth-1)),
            Box::new(boolean(r,depth-1))),
        3|4=>Boolean::Or(Box::new(boolean(r,depth-1)),
            Box::new(boolean(r,depth-1))),
        _=>Boolean::TouchCompare(Rel::generate(r),
            Box::new(integer(r,2)),Box::new(integer(r,1))),
    }
}

#[derive(Clone,Debug)]
enum Stmt {
    SetInteger(usize,Integer),
    SetBoolean(Boolean),
    Update(usize,Integer,bool),
    If(Boolean,Vec<Stmt>,Vec<Stmt>),
    /// A loop never mutates the control variable 'k' except by its final
    /// automatic increment. Nested loops are not generated.
    Repeat(usize,Vec<Stmt>),
}
impl Stmt {
    fn render(&self,c:bool,indent:usize)->String {
        let pad=" ".repeat(indent);
        match self {
            Self::SetInteger(slot,e)=>format!("{pad}{} {} {}",
                INT_NAMES[*slot],if c {"="} else {":="},e.render(c)),
            Self::SetBoolean(e)=>format!("{pad}flag {} {}",
                if c {"="} else {":="},e.render(c)),
            Self::Update(slot,e,dec)=>{
                let name=INT_NAMES[*slot];
                if c {
                    format!("{pad}{name} {} {}",if *dec {"-="} else {"+="},e.render(true))
                } else {
                    format!("{pad}{}({name}, {})",
                        if *dec {"DEC"} else {"INC"},e.render(false))
                }
            }
            Self::If(cond,a,b)=>{
                if c {
                    format!("{pad}if ({}) {{\n{}\n{pad}}} else {{\n{}\n{pad}}}",
                        cond.render(true),sequence(a,true,indent+2),
                        sequence(b,true,indent+2))
                } else {
                    format!("{pad}IF {} THEN\n{}\n{pad}ELSE\n{}\n{pad}END",
                        cond.render(false),sequence(a,false,indent+2),
                        sequence(b,false,indent+2))
                }
            }
            Self::Repeat(n,body)=>{
                if c {
                    format!("{pad}k = 0;\n{pad}do {{\n{}\n{pad}  k++;\n{pad}}} while (k < {n})",
                        sequence(body,true,indent+2))
                } else {
                    format!("{pad}k := 0;\n{pad}REPEAT\n{};\n{pad}  INC(k)\n{pad}UNTIL k >= {n}",
                        sequence(body,false,indent+2))
                }
            }
        }
    }
    fn nodes(&self)->usize {
        match self {
            Self::SetInteger(_,x)|Self::Update(_,x,_)=>1+x.nodes(),
            Self::SetBoolean(b)=>1+b.nodes(),
            Self::If(b,a,c)=>1+b.nodes()+a.iter().map(Self::nodes).sum::<usize>()
                +c.iter().map(Self::nodes).sum::<usize>(),
            Self::Repeat(_,body)=>1+body.iter().map(Self::nodes).sum::<usize>(),
        }
    }
}
fn sequence(stmts:&[Stmt],c:bool,indent:usize)->String {
    let sep=if c {";\n"} else {";\n"};
    stmts.iter().map(|s|s.render(c,indent)).collect::<Vec<_>>().join(sep)
}
fn statement(r:&mut Rng,depth:usize,allow_loop:bool)->Stmt {
    let choice=if depth==0 {r.below(3)} else {r.below(7)};
    match choice {
        0=>Stmt::SetInteger(3+r.below(3),integer(r,3)),
        1=>Stmt::SetBoolean(boolean(r,3)),
        2=>Stmt::Update(3+r.below(3),integer(r,2),r.below(2)==0),
        3|4=>Stmt::If(boolean(r,3),
            (0..1+r.below(2)).map(|_|statement(r,depth-1,false)).collect(),
            (0..1+r.below(2)).map(|_|statement(r,depth-1,false)).collect()),
        _ if allow_loop=>Stmt::Repeat(1+r.below(4),
            (0..1+r.below(3)).map(|_|statement(r,depth-1,false)).collect()),
        _=>Stmt::Update(3+r.below(3),integer(r,2),r.below(2)==0),
    }
}
#[derive(Clone,Debug)]
struct Program {body:Vec<Stmt>}
impl Program {
    fn generate(seed:u64)->Self {
        let mut r=Rng::new(seed^0xad51_712b_54e0_3317);
        let mut body=vec![
            // Mandatory nontrivial witnesses; remaining statements are
            // recursively generated, not filled into a fixed program string.
            Stmt::SetInteger(3,Integer::Binary(IOp::Div,
                Box::new(integer(&mut r,2)),Box::new(Integer::Literal(3)))),
            Stmt::SetBoolean(Boolean::And(
                Box::new(Boolean::Compare(Rel::Ne,
                    Box::new(Integer::Variable(0)),Box::new(Integer::Literal(0)))),
                Box::new(Boolean::Or(
                    Box::new(Boolean::TouchCompare(Rel::Gt,
                        Box::new(Integer::Variable(1)),
                        Box::new(Integer::Literal(0)))),
                    Box::new(boolean(&mut r,2)))))),
            Stmt::Repeat(1+r.below(3),vec![statement(&mut r,2,false)]),
        ];
        for _ in 0..5+r.below(6) {body.push(statement(&mut r,3,true));}
        Self{body}
    }
    fn source_procedure(&self,name:&str,mirror:bool)->String {
        let mut out=format!(
            "  PROCEDURE {name}*(a,b,c: INTEGER): INTEGER;\n\
             \x20 VAR x,y,acc,k: INTEGER; flag: BOOLEAN;\n\
             \x20 BEGIN\n\
             \x20   hits := 0;\n\
             \x20   x := a; y := b; acc := c; flag := FALSE;\n");
        out.push_str(&sequence(&self.body,false,4));
        out.push_str(";\n");
        // These two endings are equivalent under 32-bit wrapping arithmetic.
        if mirror {
            out.push_str("    RETURN (acc + x) + (y + hits)\n");
        } else {
            out.push_str("    RETURN ((acc + x) + y) + hits\n");
        }
        writeln!(out,"  END {name};").unwrap();
        out
    }
    fn source(&self,cases:&[(i32,i32,i32)],checks:bool)->String {
        let mut out=String::from(
            "MODULE Fuzz;\nVAR hits: INTEGER;\n\
             \x20 PROCEDURE Touch*(n:INTEGER):INTEGER;\n\
             \x20 BEGIN INC(hits); RETURN n END Touch;\n");
        out.push_str(&self.source_procedure("Probe",false));
        out.push_str(&self.source_procedure("Mirror",true));
        if checks {
            out.push_str("BEGIN\n");
            for &(a,b,c) in cases {
                writeln!(out,
                    "  IF Probe({}, {}, {}) # Mirror({}, {}, {}) THEN ASSERT(FALSE) END;",
                    literal(a),literal(b),literal(c),
                    literal(a),literal(b),literal(c)).unwrap();
            }
        }
        out.push_str("END Fuzz.\n");
        out
    }
    fn c_reference(&self,cases:&[(i32,i32,i32)],seed:u64)->String {
        let mut out=String::from(
            "#include <stdint.h>\n#include <stdio.h>\n\
             static int32_t hits;\n\
             static int32_t touch(int32_t x) { hits++; return x; }\n\
             static int32_t floor_div(int32_t a,int32_t b) {\n\
             \x20 int64_t q=(int64_t)a / b, r=(int64_t)a % b;\n\
             \x20 if (r && ((r<0)!=(b<0))) --q;\n\
             \x20 return (int32_t)q;\n}\n\
             static int32_t floor_mod(int32_t a,int32_t b) {\n\
             \x20 int64_t q=(int64_t)a / b, r=(int64_t)a % b;\n\
             \x20 if (r && ((r<0)!=(b<0))) --q;\n\
             \x20 return (int32_t)((int64_t)a-q*b);\n}\n\
             static int32_t reference_probe(int32_t a,int32_t b,int32_t c) {\n\
             \x20 int32_t x=a,y=b,acc=c,k=0; int flag=0; hits=0;\n");
        out.push_str(&sequence(&self.body,true,2));
        out.push_str(";\n  return ((acc+x)+y)+hits;\n}\n");
        out.push_str(
            "extern int32_t Fuzz__Probe(int32_t,int32_t,int32_t);\n\
             extern int32_t Fuzz__Mirror(int32_t,int32_t,int32_t);\n\
             int main(void) {\n");
        for (index,&(a,b,c)) in cases.iter().enumerate() {
            writeln!(out,
                "  {{ int32_t a={},b={},c={};\n\
                 \x20    int32_t want=reference_probe(a,b,c);\n\
                 \x20    int32_t got=Fuzz__Probe(a,b,c),mirror=Fuzz__Mirror(a,b,c);\n\
                 \x20    if (got!=want || mirror!=want) {{\n\
                 \x20      fprintf(stderr,\"typed seed {seed} case {index}: got=%d mirror=%d C=%d\\n\",got,mirror,want);\n\
                 \x20      return 1;\n\
                 \x20    }}\n  }}",
                literal(a),literal(b),literal(c)).unwrap();
        }
        writeln!(out,"  puts(\"PASS typed seed {seed}: {} C differential cases\");",cases.len()).unwrap();
        out.push_str("  return 0;\n}\n");
        out
    }
}
fn inputs(seed:u64)->Vec<(i32,i32,i32)> {
    let mut r=Rng::new(seed^0xef30_5a71_0d92_8bf1);
    let mut out=vec![
        (0,0,0),(-1,0,1),(1,-1,0),(i32::MIN,0,i32::MAX),
        (i32::MAX,i32::MIN,-1),(i32::MIN,i32::MAX,i32::MIN),
        (-37,-13,-7),(37,13,7),
    ];
    while out.len()<INPUTS {
        let val=|r:&mut Rng|->i32 {
            if r.below(3)==0 {r.range(-70,70)} else {r.next() as i32}
        };
        out.push((val(&mut r),val(&mut r),val(&mut r)));
    }
    out
}
pub(super) fn render(seed:u64,mode:&str)->Result<String,String> {
    let program=Program::generate(seed);
    let cases=inputs(seed);
    match mode {
        "typed-source"=>Ok(program.source(&cases,false)),
        "typed-selfcheck"=>Ok(program.source(&cases,true)),
        "typed-harness"=>Ok(program.c_reference(&cases,seed)),
        _=>Err(format!("unknown typed compiler subset mode {mode}")),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_recursive_ast_is_bounded_and_type_correct() {
        for seed in 0..128 {
            let p=Program::generate(seed);
            let total=p.body.iter().map(Stmt::nodes).sum::<usize>();
            assert!((5..3000).contains(&total),"seed {seed}: {total} nodes");
            let src=render(seed,"typed-source").unwrap();
            let chk=render(seed,"typed-selfcheck").unwrap();
            let c=render(seed,"typed-harness").unwrap();
            assert!(chk.starts_with(src.trim_end_matches("END Fuzz.\n")));
            assert_eq!(chk.matches("ASSERT(FALSE)").count(),INPUTS);
            assert_eq!(c.matches("reference_probe(a,b,c)").count(),INPUTS);
            assert_eq!(src,render(seed,"typed-source").unwrap());
            assert!(src.contains(" DIV "));
            assert!(src.contains("REPEAT"));
            assert!(src.contains(" OR "));
            assert!(src.contains("Touch("));
            assert!(src.contains("flag := "));
            assert!(src.contains("Mirror*("));
        }
    }
    #[test]
    fn recursive_boolean_and_integer_expression_types() {
        for seed in 0..32 {
            let mut r=Rng::new(seed);
            for ty in [Type::Integer,Type::Boolean] {
                let expr=generate(&mut r,ty,4);
                assert_eq!(expr.ty(),ty);
                assert!(expr.nodes()>=1);
                assert!(expr.nodes()<500);
                assert!(!expr.render(false).is_empty());
                assert!(!expr.render(true).is_empty());
            }
        }
    }
    #[test]
    fn source_and_c_comparison_have_common_seeded_cases() {
        for seed in [0,1,3,7,99,2026] {
            let src=render(seed,"typed-selfcheck").unwrap();
            let c=render(seed,"typed-harness").unwrap();
            assert_eq!(src.matches("ASSERT(FALSE)").count(),
                c.matches("reference_probe(a,b,c)").count());
            assert!(c.contains("floor_div("));
            assert!(c.contains("floor_mod("));
            assert!(c.contains("Fuzz__Mirror"));
        }
    }
}
