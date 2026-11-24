//! Type-directed, recursively generated Oberon differential programs.
//! No language interpreter or statically calculated expected outputs:
//! render the same typed, bounded AST as Oberon and as independent C,
//! compile both, and compare runtime results for reproducible inputs.
use super::Rng;
use std::fmt::Write as _;

const INPUTS:usize=32;
const ARRAY_LEN:usize=4;
const INT_NAMES:[&str;6]=["a","b","c","x","y","acc"];

#[cfg(test)]
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
enum Type {Integer,Boolean}
#[derive(Clone,Copy,Debug)]
enum IOp {Add,Sub,Mul,Div,Mod}
impl IOp {
    fn oberon(self)->&'static str {
        match self {Self::Add=>"+",Self::Sub=>"-",Self::Mul=>"*",
            Self::Div=>"DIV",Self::Mod=>"MOD"}
    }
    fn c_helper(self)->&'static str {
        match self {
            Self::Add=>"wrap_add",Self::Sub=>"wrap_sub",Self::Mul=>"wrap_mul",
            Self::Div=>"floor_div",Self::Mod=>"floor_mod",
        }
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
    /// A side-effect-free read from a bounded local INTEGER array.
    ArrayRead(Box<Integer>),
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
#[cfg(test)]
#[derive(Clone,Debug)]
enum TypedExpr {
    Integer(Integer),
    Boolean(Boolean),
}

fn literal(n:i32)->String {
    if n==i32::MIN {"(0 - 2147483647 - 1)".into()}
    else if n<0 {format!("({n})")} else {n.to_string()}
}
/// Any signed expression, including MIN(INTEGER), becomes an in-bounds
/// subscript using Oberon's floor MOD semantics (not C's remainder).
fn array_index(index:&Integer,c:bool)->String {
    if c {format!("floor_mod({}, {ARRAY_LEN})",index.render(true))}
    else {format!("({} MOD {ARRAY_LEN})",index.render(false))}
}
impl Integer {
    fn render(&self,c:bool)->String {
        match self {
            Self::Variable(i)=>INT_NAMES[*i].to_string(),
            Self::Literal(n)=>literal(*n),
            Self::ArrayRead(index)=>format!("cells[{}]",array_index(index,c)),
            Self::Binary(op,a,b)=>{
                let (a,b)=(a.render(c),b.render(c));
                if c {
                    // Do not rely on the C compiler's signed-overflow policy.
                    format!("{}({a}, {b})",op.c_helper())
                } else {
                    format!("({a} {} {b})",op.oberon())
                }
            }
        }
    }
    #[cfg(test)]
    fn nodes(&self)->usize {
        match self {
            Self::Variable(_)|Self::Literal(_)=>1,
            Self::ArrayRead(index)=>1+index.nodes(),
            Self::Binary(_,a,b)=>1+a.nodes()+b.nodes(),
        }
    }
}
impl Boolean {
    fn render(&self,c:bool)->String {
        match self {
            Self::Literal(v)=>{
                let value=if c {
                    if *v {"1"} else {"0"}
                } else if *v {"TRUE"} else {"FALSE"};
                value.to_string()
            },
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
    #[cfg(test)]
    fn nodes(&self)->usize {
        match self {
            Self::Literal(_)|Self::Variable=>1,
            Self::Compare(_,a,b)|Self::TouchCompare(_,a,b)=>1+a.nodes()+b.nodes(),
            Self::Not(x)=>1+x.nodes(),
            Self::And(a,b)|Self::Or(a,b)=>1+a.nodes()+b.nodes(),
        }
    }
}
#[cfg(test)]
impl TypedExpr {
    fn render(&self,c:bool)->String {
        match self {Self::Integer(e)=>e.render(c),Self::Boolean(e)=>e.render(c)}
    }
    fn ty(&self)->Type {
        match self {Self::Integer(_)=>Type::Integer,Self::Boolean(_)=>Type::Boolean}
    }
    #[cfg(test)]
    fn nodes(&self)->usize {
        match self {Self::Integer(e)=>e.nodes(),Self::Boolean(e)=>e.nodes()}
    }
}
#[cfg(test)]
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
    // Array reads are genuine recursive AST terms, not source templates.
    // Their indices recursively contain arbitrary generated INTEGER trees.
    if r.below(6)==0 {
        return Integer::ArrayRead(Box::new(integer(r,depth-1)));
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

/// An assignable checked INTEGER place. Indexed addresses are bounded and
/// side-effect free so multiple VAR actuals can be evaluated before mutation.
#[derive(Clone,Debug)]
enum Place {
    Scalar(usize),
    Indexed(Box<Integer>),
}
impl Place {
    fn render(&self,c:bool)->String {
        match self {
            Self::Scalar(slot)=>INT_NAMES[*slot].to_string(),
            Self::Indexed(index)=>format!("cells[{}]",array_index(index,c)),
        }
    }
    #[cfg(test)]
    fn nodes(&self)->usize {
        match self {Self::Scalar(_)=>1,Self::Indexed(index)=>1+index.nodes()}
    }
}
fn place(r:&mut Rng,depth:usize)->Place {
    if r.below(3)==0 {Place::Scalar(3+r.below(3))}
    else {Place::Indexed(Box::new(integer(r,depth)))}
}
/// Adding/subtracting a multiple of the array length leaves a wrapped 32-bit
/// index in the same congruence class. Since ARRAY_LEN divides 2^32,
/// even signed MIN/MAX wraparound preserves the final bounded array element.
fn equivalent_index(index:Integer,r:&mut Rng)->Integer {
    let offset=ARRAY_LEN as i32*(1+r.below(4) as i32);
    Integer::Binary(
        if r.below(2)==0 {IOp::Add} else {IOp::Sub},
        Box::new(index),Box::new(Integer::Literal(offset)))
}
/// Generate distinct places, identical ASTs, and different address
/// expressions guaranteed to alias. Index terms are pure: all actual
/// addresses/values can be evaluated before any callee mutation.
fn var_pair(r:&mut Rng)->(Place,Place) {
    match r.below(4) {
        0=>{
            let left=place(r,2);
            (left.clone(),left)
        },
        1=>{
            let index=integer(r,2);
            let alias=equivalent_index(index.clone(),r);
            (Place::Indexed(Box::new(index)),Place::Indexed(Box::new(alias)))
        },
        _=>(place(r,2),place(r,2)),
    }
}
#[derive(Clone,Debug)]
enum Stmt {
    SetInteger(usize,Integer),
    SetBoolean(Boolean),
    Update(usize,Integer,bool),
    SetArray(Integer,Integer),
    UpdateArray(Integer,Integer,bool),
    /// Scalar or bounded indexed actual; the value arguments can read arrays.
    CallVar(Place,Integer,Boolean),
    /// Two independently addressable actuals, deliberately sometimes aliased.
    CallVarAlias(Place,Place,Integer),
    /// Mutate a checked BOOLEAN lvalue with a short-circuit guard.
    CallVarBoolean(Boolean),
    If(Boolean,Vec<Stmt>,Vec<Stmt>),
    /// A loop never mutates the control variable 'k' except by its final
    /// automatic increment. Nested loops are not generated.
    Repeat(usize,Vec<Stmt>),
}
impl Stmt {
    fn render(&self,c:bool,indent:usize)->String {
        self.render_with_link(c,indent,false)
    }
    fn render_with_link(&self,c:bool,indent:usize,imported:bool)->String {
        let pad=" ".repeat(indent);
        match self {
            Self::SetInteger(slot,e)=>format!("{pad}{} {} {}",
                INT_NAMES[*slot],if c {"="} else {":="},e.render(c)),
            Self::SetBoolean(e)=>format!("{pad}flag {} {}",
                if c {"="} else {":="},e.render(c)),
            Self::Update(slot,e,dec)=>{
                let name=INT_NAMES[*slot];
                if c {
                    format!("{pad}{name} = {}({name}, {})",
                        if *dec {"wrap_sub"} else {"wrap_add"},e.render(true))
                } else {
                    format!("{pad}{}({name}, {})",
                        if *dec {"DEC"} else {"INC"},e.render(false))
                }
            }
            Self::SetArray(index,value)=>format!("{pad}cells[{}] {} {}",
                array_index(index,c),if c {"="} else {":="},value.render(c)),
            Self::UpdateArray(index,delta,dec)=>{
                let target=format!("cells[{}]",array_index(index,c));
                if c {
                    // Index is a pure typed expression; duplicated C rendering
                    // cannot repeat any Touch side effect.
                    format!("{pad}{target} = {}({target}, {})",
                        if *dec {"wrap_sub"} else {"wrap_add"},delta.render(true))
                } else {
                    format!("{pad}{}({target}, {})",
                        if *dec {"DEC"} else {"INC"},delta.render(false))
                }
            }
            Self::CallVar(target,delta,minus)=>{
                let target=target.render(c);
                if c {
                    format!("{pad}acc = change(&{target}, {}, {})",
                        delta.render(true),minus.render(true))
                } else {
                    let callee=if imported {"Ops.Change"} else {"Change"};
                    format!("{pad}acc := {callee}({target}, {}, {})",
                        delta.render(false),minus.render(false))
                }
            }
            Self::CallVarAlias(left,right,delta)=>{
                let left=left.render(c);
                let right=right.render(c);
                if c {
                    format!("{pad}acc = twist(&{left}, &{right}, {})",delta.render(true))
                } else {
                    let callee=if imported {"Ops.Twist"} else {"Twist"};
                    format!("{pad}acc := {callee}({left}, {right}, {})",delta.render(false))
                }
            }
            Self::CallVarBoolean(guard)=>{
                if c {
                    format!("{pad}flag = flip_flag(&flag, {})",guard.render(true))
                } else {
                    let callee=if imported {"Ops.FlipFlag"} else {"FlipFlag"};
                    format!("{pad}flag := {callee}(flag, {})",guard.render(false))
                }
            }
            Self::If(cond,a,b)=>{
                if c {
                    format!("{pad}if ({}) {{\n{};\n{pad}}} else {{\n{};\n{pad}}}",
                        cond.render(true),sequence_with_link(a,true,indent+2,imported),
                        sequence_with_link(b,true,indent+2,imported))
                } else {
                    format!("{pad}IF {} THEN\n{}\n{pad}ELSE\n{}\n{pad}END",
                        cond.render(false),sequence_with_link(a,false,indent+2,imported),
                        sequence_with_link(b,false,indent+2,imported))
                }
            }
            Self::Repeat(n,body)=>{
                if c {
                    format!("{pad}k = 0;\n{pad}do {{\n{};\n{pad}  k++;\n{pad}}} while (k < {n})",
                        sequence_with_link(body,true,indent+2,imported))
                } else {
                    format!("{pad}k := 0;\n{pad}REPEAT\n{};\n{pad}  INC(k)\n{pad}UNTIL k >= {n}",
                        sequence_with_link(body,false,indent+2,imported))
                }
            }
        }
    }
    #[cfg(test)]
    fn nodes(&self)->usize {
        match self {
            Self::SetInteger(_,x)|Self::Update(_,x,_)=>1+x.nodes(),
            Self::SetBoolean(b)=>1+b.nodes(),
            Self::SetArray(index,value)|Self::UpdateArray(index,value,_)=>
                1+index.nodes()+value.nodes(),
            Self::CallVar(target,delta,minus)=>
                1+target.nodes()+delta.nodes()+minus.nodes(),
            Self::CallVarAlias(left,right,delta)=>
                1+left.nodes()+right.nodes()+delta.nodes(),
            Self::CallVarBoolean(guard)=>1+guard.nodes(),
            Self::If(b,a,c)=>1+b.nodes()+a.iter().map(Self::nodes).sum::<usize>()
                +c.iter().map(Self::nodes).sum::<usize>(),
            Self::Repeat(_,body)=>1+body.iter().map(Self::nodes).sum::<usize>(),
        }
    }
}
fn sequence(stmts:&[Stmt],c:bool,indent:usize)->String {
    stmts.iter().map(|s|s.render(c,indent)).collect::<Vec<_>>().join(";\n")
}
fn sequence_with_link(stmts:&[Stmt],c:bool,indent:usize,imported:bool)->String {
    if !imported { return sequence(stmts,c,indent); }
    stmts.iter().map(|s|s.render_with_link(c,indent,imported))
        .collect::<Vec<_>>().join(";\n")
}
fn statement(r:&mut Rng,depth:usize,allow_loop:bool)->Stmt {
    let choice=if depth==0 {r.below(10)} else {r.below(15)};
    match choice {
        0=>Stmt::SetInteger(3+r.below(3),integer(r,3)),
        1=>Stmt::SetBoolean(boolean(r,3)),
        2=>Stmt::Update(3+r.below(3),integer(r,2),r.below(2)==0),
        3=>Stmt::SetArray(integer(r,2),integer(r,3)),
        4=>Stmt::UpdateArray(integer(r,2),integer(r,2),r.below(2)==0),
        5|6=>Stmt::CallVar(place(r,2),integer(r,2),boolean(r,2)),
        7=>Stmt::CallVarBoolean(boolean(r,3)),
        8=>Stmt::CallVar(Place::Indexed(Box::new(integer(r,2))),
            integer(r,2),boolean(r,2)),
        9=>{
            let (left,right)=var_pair(r);
            Stmt::CallVarAlias(left,right,integer(r,2))
        },
        10|11=>Stmt::If(boolean(r,3),
            (0..1+r.below(2)).map(|_|statement(r,depth-1,false)).collect(),
            (0..1+r.below(2)).map(|_|statement(r,depth-1,false)).collect()),
        _ if allow_loop=>Stmt::Repeat(1+r.below(4),
            (0..1+r.below(3)).map(|_|statement(r,depth-1,false)).collect()),
        _=>Stmt::UpdateArray(integer(r,2),integer(r,2),r.below(2)==0),
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
            Stmt::Update(4,Integer::Binary(IOp::Mod,
                Box::new(Integer::Variable(2)),
                Box::new(Integer::Literal(-5))),false),
            Stmt::SetBoolean(Boolean::And(
                Box::new(Boolean::Compare(Rel::Ne,
                    Box::new(Integer::Variable(0)),Box::new(Integer::Literal(0)))),
                Box::new(Boolean::Or(
                    Box::new(Boolean::TouchCompare(Rel::Gt,
                        Box::new(Integer::Variable(1)),
                        Box::new(Integer::Literal(0)))),
                    Box::new(boolean(&mut r,2)))))),
            Stmt::Repeat(1+r.below(3),vec![statement(&mut r,2,false)]),
            // Required read/write/indexed-update witnesses for every seed.
            // Further array operations are chosen recursively by the AST.
            Stmt::SetArray(Integer::Literal(0),Integer::Variable(0)),
            Stmt::UpdateArray(Integer::Literal(3),Integer::Literal(1),false),
            Stmt::SetInteger(3,Integer::ArrayRead(Box::new(Integer::Variable(1)))),
            // Mandatory scalar-by-reference calls, including an array
            // value expression and a side-effectful BOOLEAN actual.
            Stmt::CallVar(Place::Scalar(5),Integer::Variable(0),Boolean::Variable),
            Stmt::CallVar(Place::Scalar(3),
                Integer::ArrayRead(Box::new(Integer::Variable(1))),
                Boolean::And(
                    Box::new(Boolean::Literal(true)),
                    Box::new(Boolean::TouchCompare(Rel::Ne,
                        Box::new(Integer::Variable(2)),
                        Box::new(Integer::Literal(0)))))),
            // Required indexed VAR and both aliased and distinct VAR pairs.
            Stmt::CallVar(Place::Indexed(Box::new(Integer::Variable(0))),
                Integer::Variable(1),Boolean::Variable),
            Stmt::CallVarAlias(Place::Scalar(3),Place::Scalar(3),
                Integer::Variable(2)),
            Stmt::CallVarAlias(Place::Scalar(3),Place::Scalar(4),
                Integer::Literal(1)),
            // Same-address and distinct array elements, and a mixed
            // scalar/indexed pair with a nested array read as subscript.
            Stmt::CallVarAlias(
                Place::Indexed(Box::new(Integer::Variable(0))),
                Place::Indexed(Box::new(Integer::Variable(0))),
                Integer::Literal(7)),
            Stmt::CallVarAlias(
                Place::Indexed(Box::new(Integer::Literal(0))),
                Place::Indexed(Box::new(Integer::Literal(3))),
                Integer::Literal(-1)),
            Stmt::CallVarAlias(
                Place::Scalar(4),
                Place::Indexed(Box::new(Integer::ArrayRead(
                    Box::new(Integer::Variable(0))))),
                Integer::Variable(2)),
            // Distinct source expressions resolve to the same VAR address
            // modulo four, including 32-bit MIN/MAX wrapping. The delta
            // reads an aliased element before the callee changes it.
            Stmt::CallVarAlias(
                Place::Indexed(Box::new(Integer::Variable(0))),
                Place::Indexed(Box::new(Integer::Binary(IOp::Add,
                    Box::new(Integer::Variable(0)),
                    Box::new(Integer::Literal(ARRAY_LEN as i32))))),
                Integer::ArrayRead(Box::new(Integer::Variable(0)))),
            // Nested index reads also alias even with an offset expression.
            Stmt::CallVarAlias(
                Place::Indexed(Box::new(Integer::ArrayRead(
                    Box::new(Integer::Variable(1))))),
                Place::Indexed(Box::new(Integer::Binary(IOp::Sub,
                    Box::new(Integer::ArrayRead(
                        Box::new(Integer::Variable(1)))),
                    Box::new(Integer::Literal(ARRAY_LEN as i32))))),
                Integer::Literal(3)),
            // A mandatory VAR BOOLEAN mutation with a side-effectful
            // short-circuit guard, including a possible skipped Touch.
            Stmt::CallVarBoolean(Boolean::Or(
                Box::new(Boolean::Variable),
                Box::new(Boolean::TouchCompare(Rel::Ne,
                    Box::new(Integer::Variable(0)),
                    Box::new(Integer::Literal(0)))))),
            // Guarantee a use of the BOOLEAN local so both languages must
            // preserve its value, and C -Werror never rejects an unused flag.
            Stmt::If(Boolean::Variable,
                vec![Stmt::Update(5,Integer::Literal(1),false)],
                vec![Stmt::Update(5,Integer::Literal(1),true)]),
        ];
        for _ in 0..5+r.below(6) {body.push(statement(&mut r,3,true));}
        Self{body}
    }
    fn source_procedure(&self,name:&str,mirror:bool,imported:bool)->String {
        let mut out=format!(
            "  PROCEDURE {name}*(a,b,c: INTEGER): INTEGER;\n\
             \x20 VAR x,y,acc,k: INTEGER; flag: BOOLEAN;\n\
             \x20     cells: ARRAY 4 OF INTEGER;\n\
             \x20 BEGIN\n\
             \x20   hits := 0;\n\
             \x20   x := a; y := b; acc := c; flag := FALSE;\n\
             \x20   cells[0] := a; cells[1] := b; cells[2] := c; cells[3] := 0;\n");
        out.push_str(&sequence_with_link(&self.body,false,4,imported));
        out.push_str(";\n");
        // These two endings are equivalent under 32-bit wrapping arithmetic.
        if mirror {
            out.push_str("    RETURN ((acc + cells[0]) + (x + y)) + ((hits + cells[1]) + (cells[2] + cells[3]))\n");
        } else {
            out.push_str("    RETURN (((((((acc + x) + y) + hits) + cells[0]) + cells[1]) + cells[2]) + cells[3])\n");
        }
        writeln!(out,"  END {name};").unwrap();
        out
    }
    fn source(&self,cases:&[(i32,i32,i32)],checks:bool)->String {
        self.source_mode(cases,checks,false)
    }
    fn source_mode(&self,cases:&[(i32,i32,i32)],checks:bool,imported:bool)->String {
        let mut out=String::from("MODULE Fuzz;\n");
        if imported { out.push_str("IMPORT Ops := FuzzOps;\n"); }
        out.push_str(r#"VAR hits: INTEGER;
  PROCEDURE Touch*(n:INTEGER):INTEGER;
  BEGIN INC(hits); RETURN n END Touch;
"#);
        if !imported {out.push_str(&Self::helpers(false));}
        out.push_str(&self.source_procedure("Probe",false,imported));
        out.push_str(&self.source_procedure("Mirror",true,imported));
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
    /// Keep local/imported Oberon helper bodies identical; the C oracle
    /// remains an independent implementation and separate compilation.
    fn helpers(exported:bool)->String {
        let star=if exported {"*"} else {""};
        format!(r#"  PROCEDURE Change{star}(VAR n: INTEGER; delta: INTEGER; minus: BOOLEAN): INTEGER;
   BEGIN
     IF minus THEN DEC(n,delta) ELSE INC(n,delta) END;
     RETURN n
   END Change;
   PROCEDURE FlipFlag{star}(VAR b: BOOLEAN; guard: BOOLEAN): BOOLEAN;
   BEGIN
     IF guard THEN b := ~b END;
     RETURN b
   END FlipFlag;
   PROCEDURE Twist{star}(VAR left,right: INTEGER; delta: INTEGER): INTEGER;
   BEGIN
     INC(left,delta);
     right := left + right;
     DEC(left);
     RETURN right
   END Twist;
"#)
    }
    fn provider()->String {
        format!("MODULE FuzzOps;\n{}END FuzzOps.\n",Self::helpers(true))
    }
    fn c_reference(&self,cases:&[(i32,i32,i32)],seed:u64)->String {
        let mut out=String::from(
            "#include <stdint.h>\n#include <stdio.h>\n\
             static int32_t hits;\n\
             /* Explicit mod-2^32 operations are defined even under -O2
                and without -fwrapv. Convert unsigned bits to signed via
                an in-range int64_t intermediate (no implementation-defined
                narrowing of out-of-range uint32_t values). */\n\
             static inline int32_t from_bits(uint32_t bits) {\n\
             \x20 int64_t signed_value=(int64_t)bits;\n\
             \x20 if (signed_value > INT32_MAX) signed_value -= 4294967296LL;\n\
             \x20 return (int32_t)signed_value;\n}\n\
             static inline int32_t wrap_add(int32_t a,int32_t b) {\n\
             \x20 return from_bits((uint32_t)a + (uint32_t)b);\n}\n\
             static inline int32_t wrap_sub(int32_t a,int32_t b) {\n\
             \x20 return from_bits((uint32_t)a - (uint32_t)b);\n}\n\
             static inline int32_t wrap_mul(int32_t a,int32_t b) {\n\
             \x20 return from_bits((uint32_t)a * (uint32_t)b);\n}\n\
             static int32_t touch(int32_t x) { hits++; return x; }\n\
             static int32_t floor_div(int32_t a,int32_t b) {\n\
             \x20 int64_t q=(int64_t)a / b, r=(int64_t)a % b;\n\
             \x20 if (r && ((r<0)!=(b<0))) --q;\n\
             \x20 return (int32_t)q;\n}\n\
             static int32_t floor_mod(int32_t a,int32_t b) {\n\
             \x20 int64_t q=(int64_t)a / b, r=(int64_t)a % b;\n\
             \x20 if (r && ((r<0)!=(b<0))) --q;\n\
             \x20 return (int32_t)((int64_t)a-q*b);\n}\n\
             static int32_t change(int32_t *n,int32_t delta,int minus) {\n\
             \x20 *n=minus ? wrap_sub(*n,delta) : wrap_add(*n,delta);\n\
             \x20 return *n;\n}\n\
             static int flip_flag(int *b,int guard) {\n\
             \x20 if (guard) *b=!*b;\n\
             \x20 return *b;\n}\n\
             static int32_t twist(int32_t *left,int32_t *right,int32_t delta) {\n\
             \x20 *left=wrap_add(*left,delta);\n\
             \x20 *right=wrap_add(*left,*right);\n\
             \x20 *left=wrap_sub(*left,1);\n\
             \x20 return *right;\n\
}\n\
             static int32_t reference_probe(int32_t a,int32_t b,int32_t c) {\n\
             \x20 int32_t x=a,y=b,acc=c,k=0; int flag=0; hits=0;\n\
             \x20 int32_t cells[4]={a,b,c,0};\n");
        out.push_str(&sequence(&self.body,true,2));
        out.push_str(";\n  return wrap_add(wrap_add(wrap_add(wrap_add(wrap_add(wrap_add(wrap_add(acc,x),y),hits),cells[0]),cells[1]),cells[2]),cells[3]);\n}\n");
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
        "typed-harness"|"typed-import-harness"=>Ok(program.c_reference(&cases,seed)),
        "typed-import-provider"=>Ok(Program::provider()),
        "typed-import-source"=>Ok(program.source_mode(&cases,false,true)),
        "typed-import-selfcheck"=>Ok(program.source_mode(&cases,true,true)),
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
            assert!(src.contains("cells: ARRAY 4 OF INTEGER"));
            assert!(src.contains("INC(cells["));
            assert!(src.contains("cells[("));
            assert!(src.contains("PROCEDURE Change(VAR n: INTEGER; delta: INTEGER; minus: BOOLEAN): INTEGER"));
            assert!(src.contains("Change(acc, a, flag)"));
            assert!(src.contains("Change(x, cells["));
            assert!(src.contains("PROCEDURE FlipFlag(VAR b: BOOLEAN; guard: BOOLEAN): BOOLEAN"));
            assert!(src.contains("flag := FlipFlag(flag, "));
            assert!(c.contains("flip_flag(&flag, "));
            assert!(c.contains("int32_t cells[4]={a,b,c,0};"));
            assert!(c.contains("static int32_t change(int32_t *n,int32_t delta,int minus)"));
        }
    }
    #[test]
    fn c_oracle_arithmetic_has_defined_32_bit_wrapping() {
        // These exact expressions must not become signed C operators:
        // test inputs deliberately include MIN/MAX and overflow.
        for (op,helper) in [
            (IOp::Add,"wrap_add"),(IOp::Sub,"wrap_sub"),
            (IOp::Mul,"wrap_mul"),(IOp::Div,"floor_div"),
            (IOp::Mod,"floor_mod"),
        ] {
            let expr=Integer::Binary(op,Box::new(Integer::Variable(0)),
                Box::new(Integer::Variable(1)));
            assert_eq!(expr.render(true),format!("{helper}(a, b)"));
            assert_eq!(expr.render(false),
                format!("(a {} b)",op.oberon()));
        }
        for seed in [0,1,3,7,99,2026] {
            let c=render(seed,"typed-harness").unwrap();
            for helper in ["from_bits","wrap_add","wrap_sub","wrap_mul"] {
                assert!(c.contains(&format!("static inline int32_t {helper}(")));
            }
            assert!(c.contains("return wrap_add(wrap_add(wrap_add(wrap_add(wrap_add(wrap_add(wrap_add(acc,x),y),hits),cells[0]),cells[1]),cells[2]),cells[3]);"));
            assert!(!c.contains("return ((acc+x)+y)+hits;"));
        }
        let increment=Stmt::Update(3,Integer::Literal(i32::MAX),false);
        let decrement=Stmt::Update(3,Integer::Literal(i32::MAX),true);
        assert_eq!(increment.render(true,0),"x = wrap_add(x, 2147483647)");
        assert_eq!(decrement.render(true,0),"x = wrap_sub(x, 2147483647)");
    }
    #[test]
    fn recursively_generated_array_accesses_are_bounded_and_pure() {
        let index=Integer::ArrayRead(Box::new(Integer::Variable(0)));
        assert_eq!(index.render(false),"cells[(a MOD 4)]");
        assert_eq!(index.render(true),"cells[floor_mod(a, 4)]");
        let store=Stmt::SetArray(index.clone(),Integer::Literal(i32::MIN));
        let update=Stmt::UpdateArray(index,Integer::Literal(1),true);
        assert!(store.render(false,0).contains("cells[(cells[(a MOD 4)] MOD 4)] :="));
        assert!(store.render(true,0).contains("cells[floor_mod(cells[floor_mod(a, 4)], 4)] ="));
        assert!(update.render(false,0).starts_with("DEC(cells["));
        assert!(update.render(true,0).contains("wrap_sub("));
        for seed in 0..128 {
            let oberon=render(seed,"typed-source").unwrap();
            let c=render(seed,"typed-harness").unwrap();
            assert!(oberon.contains("cells[("));
            assert!(c.contains("floor_mod("));
        }
    }
    #[test]
    fn var_parameter_calls_cover_scalar_lvalues_and_array_values() {
        let scalar=Stmt::CallVar(Place::Scalar(3),Integer::Literal(1),
            Boolean::Literal(false));
        assert_eq!(scalar.render(false,0),"acc := Change(x, 1, FALSE)");
        assert_eq!(scalar.render(true,0),"acc = change(&x, 1, 0)");
        // Recursive array reads are allowed as value actuals as well as
        // addressable bounded array elements in VAR positions.
        let array_value=Stmt::CallVar(Place::Scalar(4),
            Integer::ArrayRead(Box::new(Integer::ArrayRead(
                Box::new(Integer::Variable(0))))),Boolean::Variable);
        assert_eq!(array_value.render(false,0),
            "acc := Change(y, cells[(cells[(a MOD 4)] MOD 4)], flag)");
        assert_eq!(array_value.render(true,0),
            "acc = change(&y, cells[floor_mod(cells[floor_mod(a, 4)], 4)], flag)");
        for seed in 0..128 {
            let p=Program::generate(seed);
            let src=p.source(&inputs(seed),false);
            let c=p.c_reference(&inputs(seed),seed);
            assert!(src.contains("Change(acc, a, flag)"));
            assert!(src.contains("Change(x, cells["));
            assert!(c.contains("change(&acc, a, flag)"));
            assert!(c.contains("change(&x, cells["));
        }
    }
    #[test]
    fn generated_var_boolean_mutation_preserves_short_circuit_effects() {
        let guard=Boolean::Or(
            Box::new(Boolean::Variable),
            Box::new(Boolean::TouchCompare(Rel::Eq,
                Box::new(Integer::Variable(0)),
                Box::new(Integer::Literal(0)))));
        let call=Stmt::CallVarBoolean(guard);
        assert_eq!(call.render(false,0),
            "flag := FlipFlag(flag, (flag OR (Touch(a) = 0)))");
        assert_eq!(call.render(true,0),
            "flag = flip_flag(&flag, (flag || (touch(a) == 0)))");
        for seed in 0..128 {
            let source=render(seed,"typed-source").unwrap();
            let oracle=render(seed,"typed-harness").unwrap();
            assert!(source.contains("PROCEDURE FlipFlag(VAR b: BOOLEAN; guard: BOOLEAN): BOOLEAN"));
            assert!(source.contains("flag := FlipFlag(flag, "));
            assert!(oracle.contains("static int flip_flag(int *b,int guard)"));
            assert!(oracle.contains("flag = flip_flag(&flag, "));
        }
    }
    #[test]
    fn imported_var_calls_cover_aliasing_indexed_lvalues_and_boolean() {
        let provider=render(3,"typed-import-provider").unwrap();
        assert!(provider.starts_with("MODULE FuzzOps;"));
        assert!(provider.contains("PROCEDURE Change*(VAR n: INTEGER"));
        assert!(provider.contains("PROCEDURE FlipFlag*(VAR b: BOOLEAN"));
        assert!(provider.contains("PROCEDURE Twist*(VAR left,right: INTEGER"));
        assert_eq!(provider,render(999,"typed-import-provider").unwrap());
        let local=render(3,"typed-source").unwrap();
        for line in ["INC(left,delta);","right := left + right;",
            "DEC(left);","RETURN right"] {
            assert!(provider.contains(line));
            assert!(local.contains(line));
        }
        assert!(provider.contains("PROCEDURE Twist*(VAR left,right: INTEGER"));
        assert!(local.contains("PROCEDURE Twist(VAR left,right: INTEGER"));
        assert!(!local.contains("PROCEDURE Twist*(VAR left,right: INTEGER"));
        let oracle=render(3,"typed-harness").unwrap();
        assert!(oracle.contains("*right=wrap_add(*left,*right);"));
        assert!(oracle.contains("*left=wrap_sub(*left,1);"));
        assert!(oracle.contains("return *right;"));
        let indexed=Stmt::CallVar(
            Place::Indexed(Box::new(Integer::Variable(0))),
            Integer::Variable(1),Boolean::Variable);
        assert_eq!(indexed.render_with_link(false,0,true),
            "acc := Ops.Change(cells[(a MOD 4)], b, flag)");
        assert_eq!(indexed.render(true,0),
            "acc = change(&cells[floor_mod(a, 4)], b, flag)");
        let alias=Stmt::CallVarAlias(
            Place::Scalar(3),Place::Scalar(3),Integer::Variable(2));
        assert_eq!(alias.render_with_link(false,0,true),"acc := Ops.Twist(x, x, c)");
        assert_eq!(alias.render(true,0),"acc = twist(&x, &x, c)");
        for seed in 0..128 {
            let src=render(seed,"typed-import-source").unwrap();
            let selfcheck=render(seed,"typed-import-selfcheck").unwrap();
            let oracle=render(seed,"typed-import-harness").unwrap();
            assert!(src.starts_with("MODULE Fuzz;\nIMPORT Ops := FuzzOps;"));
            assert!(!src.contains("  PROCEDURE Change("));
            assert!(src.contains("Ops.Change(acc, a, flag)"));
            assert!(src.contains("Ops.Change(cells[(a MOD 4)], b, flag)"));
            assert!(src.contains("Ops.Twist(x, x, c)"));
            assert!(src.contains("Ops.Twist(x, y, 1)"));
            assert!(src.contains("Ops.Twist(cells[(a MOD 4)], cells[(a MOD 4)], 7)"));
            assert!(src.contains("Ops.Twist(cells[(0 MOD 4)], cells[(3 MOD 4)], (-1))"));
            assert!(src.contains("Ops.Twist(y, cells[(cells[(a MOD 4)] MOD 4)], c)"));
            assert!(src.contains("Ops.Twist(cells[(a MOD 4)], cells[((a + 4) MOD 4)], cells[(a MOD 4)])"));
            assert!(src.contains("Ops.Twist(cells[(cells[(b MOD 4)] MOD 4)], cells[((cells[(b MOD 4)] - 4) MOD 4)], 3)"));
            assert!(src.contains("Ops.FlipFlag(flag, "));
            assert_eq!(selfcheck.matches("ASSERT(FALSE)").count(),INPUTS);
            assert!(selfcheck.starts_with(src.trim_end_matches("END Fuzz.\n")));
            assert!(oracle.contains("twist(&x, &x, c)"));
            assert!(oracle.contains("twist(&cells[floor_mod(a, 4)], &cells[floor_mod(a, 4)], 7)"));
            assert!(oracle.contains("twist(&cells[floor_mod(0, 4)], &cells[floor_mod(3, 4)], (-1))"));
            assert!(oracle.contains("twist(&cells[floor_mod(a, 4)], &cells[floor_mod(wrap_add(a, 4), 4)], cells[floor_mod(a, 4)])"));
            assert!(oracle.contains("twist(&cells[floor_mod(cells[floor_mod(b, 4)], 4)], &cells[floor_mod(wrap_sub(cells[floor_mod(b, 4)], 4), 4)], 3)"));
            assert!(oracle.contains("change(&cells[floor_mod(a, 4)], b, flag)"));
            assert_eq!(oracle,render(seed,"typed-harness").unwrap());
            assert_eq!(src,render(seed,"typed-import-source").unwrap());
        }
    }
    #[test]
    fn distinct_equivalent_indices_alias_even_across_signed_wraparound() {
        // This checks the generator's congruence rule, not interpreter-
        // derived expected program outputs. Program results still come from
        // the separately compiled C oracle.
        for n in [
            i32::MIN,i32::MIN+1,-999,-7,-1,0,1,3,4,999,i32::MAX-1,i32::MAX
        ] {
            for multiple in 1..=4i32 {
                let offset=ARRAY_LEN as i32*multiple;
                assert_eq!(n.rem_euclid(ARRAY_LEN as i32),
                    n.wrapping_add(offset).rem_euclid(ARRAY_LEN as i32));
                assert_eq!(n.rem_euclid(ARRAY_LEN as i32),
                    n.wrapping_sub(offset).rem_euclid(ARRAY_LEN as i32));
            }
        }
        let left=Integer::Variable(0);
        let mut rng=Rng::new(9);
        for _ in 0..32 {
            let right=equivalent_index(left.clone(),&mut rng);
            assert_ne!(left.render(false),right.render(false));
            assert!(right.render(false).contains(" + ") ||
                right.render(false).contains(" - "));
        }
        for seed in 0..128 {
            let src=render(seed,"typed-import-source").unwrap();
            let c=render(seed,"typed-import-harness").unwrap();
            assert!(src.contains("Ops.Twist(cells[(a MOD 4)], cells[((a + 4) MOD 4)], cells[(a MOD 4)])"));
            assert!(c.contains("twist(&cells[floor_mod(a, 4)], &cells[floor_mod(wrap_add(a, 4), 4)], cells[floor_mod(a, 4)])"));
        }
    }
    #[test]
    fn all_var_place_classes_remain_reproducible_and_bounded() {
        let nested=Place::Indexed(Box::new(Integer::ArrayRead(
            Box::new(Integer::Variable(0)))));
        assert_eq!(nested.render(false),"cells[(cells[(a MOD 4)] MOD 4)]");
        assert_eq!(nested.render(true),"cells[floor_mod(cells[floor_mod(a, 4)], 4)]");
        let pair=Stmt::CallVarAlias(nested.clone(),nested,
            Integer::Literal(i32::MIN));
        assert_eq!(pair.render_with_link(false,0,true),
            "acc := Ops.Twist(cells[(cells[(a MOD 4)] MOD 4)], cells[(cells[(a MOD 4)] MOD 4)], (0 - 2147483647 - 1))");
        assert_eq!(pair.render(true,0),
            "acc = twist(&cells[floor_mod(cells[floor_mod(a, 4)], 4)], &cells[floor_mod(cells[floor_mod(a, 4)], 4)], (0 - 2147483647 - 1))");
        assert_eq!(pair.nodes(),8);
        for seed in 0..128 {
            let a=Program::generate(seed);
            assert!(a.body.iter().any(|s|matches!(s,Stmt::CallVarAlias(
                Place::Indexed(_),Place::Indexed(_),_))));
            assert!(a.body.iter().any(|s|matches!(s,Stmt::CallVarAlias(
                Place::Scalar(_),Place::Indexed(_),_))));
            assert_eq!(render(seed,"typed-source").unwrap(),
                render(seed,"typed-source").unwrap());
            assert_eq!(render(seed,"typed-import-harness").unwrap(),
                render(seed,"typed-harness").unwrap());
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
