//! Backend-independent projection of checked Oberon procedures.
//!
//! Only syntax and semantic-analysis crates are dependencies. Unsupported
//! constructs fail closed; the consuming compiler owns SSA and code generation.
use std::collections::HashMap;
use syntax::ast::{BinOp, Expr as OberonExpr, ExprKind, Stmt as OberonStmt,
                  StmtKind, TypeExpr, UnOp};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Module {
    pub name: String,
    /// Module-level INTEGER objects, in source order.
    pub globals: Vec<Global>,
    pub global_arrays: Vec<ArrayGlobal>,
    pub functions: Vec<Function>,
    /// Checked module BEGIN body; emitted as a private constructor.
    pub initializer: Option<Function>,
}

/// Exported module objects are linker-visible; private objects keep local
/// linkage. The bridge currently supports zero-initialized INTEGER only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Global {
    pub name: String,
    pub exported: bool,
}

/// Zero-initialized, bounds-checked module INTEGER array.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArrayGlobal {
    pub name: String,
    pub length: u32,
    pub exported: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Function {
    pub name: String,
    /// Value INTEGER parameters, in source order.
    pub parameters: Vec<String>,
    /// Zero-initialized INTEGER locals, in source order.
    pub locals: Vec<String>,
    /// Fixed-size single-dimensional local INTEGER arrays. Nonzero lengths.
    pub arrays: Vec<ArrayLocal>,
    /// Module arrays follow locals in array-index numbering.
    pub global_arrays: Vec<ArrayGlobal>,
    /// Ordered module globals. Variable indexes after parameters and
    /// local scalar variables refer to this list (never to frame slots).
    pub globals: Vec<String>,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArrayLocal {
    pub name: String,
    pub length: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expr {
    Variable(usize),
    /// Index into Function.arrays; the subscript is a checked INTEGER expr.
    ArrayElement { array: usize, index: Box<Expr> },
    /// Source-checked private record leaf: exact byte offset from its
    /// existing declared local/global storage root, with width 8 or 32.
    /// No free pointer, by-value record ABI or arbitrary byte access.
    RecordField { array: usize, byte_offset: u32, bits: u8 },
    Integer(i32),
    Boolean(bool),
    Binary(Op, Box<Expr>, Box<Expr>),
    /// Checked same-module INTEGER function call, including recursion.
    Call { name:String, args:Vec<Expr> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Add, Sub, Mul,
    Eq, Ne, Lt, Le, Gt, Ge,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stmt {
    Assign { variable: usize, value: Expr },
    AssignArray { array: usize, index: Expr, value: Expr },
    /// Ordered byte-exact private record leaf write; widths 8/32 only.
    AssignRecordField { array: usize, byte_offset: u32, bits: u8, value: Expr },
    Return(Expr),
    /// Evaluate a procedure call for its effect, ignoring its result.
    Call(Expr),
    If { condition: Expr, then_body: Vec<Stmt>, else_body: Vec<Stmt> },
    While { condition: Expr, body: Vec<Stmt> },
}

// Private homogeneous INTEGER records now use contiguous checked memory,
// with 4-byte fields and an addressable allocation root. No record ABI,
// pointer exposure, record extension or heterogeneous fields are inferred.
fn record_fields(ty: &TypeExpr, types: &HashMap<String, Vec<String>>)
    -> Result<Option<Vec<String>>, String>
{
    match ty {
        TypeExpr::Named(q, _) if q.len()==1 => Ok(types.get(&q[0]).cloned()),
        TypeExpr::Record { base, fields, .. } => {
            // The full Oberon checker establishes extension legality.
            // Prefix fields keep the base record's original byte offsets;
            // every represented field is still exactly four bytes.
            let mut result=if let Some(base)=base {
                if base.len()!=1 {
                    return Err("qualified base-record types need import support".into());
                }
                types.get(&base[0]).cloned()
                    .ok_or("unsupported record extension base")?
            } else { Vec::new() };
            for field in fields {
                let nested=if matches!(&field.ty,
                    TypeExpr::Named(q, _) if q.as_slice()==["INTEGER"] || q.as_slice()==["BYTE"]) {
                    None
                } else {
                    Some(record_fields(&field.ty, types)?
                        .ok_or("record fields must be INTEGER, BYTE or supported private records")?)
                };
                for (name, _) in &field.names {
                    // Oberon accepts an extension that hides a base field,
                    // with a diagnostic. Our compact leaf-name projection
                    // cannot distinguish the two nominal declarations:
                    // silently picking the first offset would miscompile.
                    // Reject until source field declaration IDs are retained.
                    if result.iter().any(|path|
                        path.split('.').next() == Some(name.as_str())) {
                        return Err(format!(
                            "record field {name} hides another field; native layout needs declaration identity"));
                    }
                    if let Some(paths)=&nested {
                        for path in paths {
                            // Dots are not legal Oberon identifier characters.
                            // Hence this path is unambiguous for nested fields.
                            result.push(format!("{name}.{path}"));
                        }
                    } else {
                        result.push(name.clone());
                    }
                    if result.len()>1024 {
                        return Err("native INTEGER record exceeds 1024 words".into());
                    }
                }
            }
            if result.is_empty() {
                return Err("native INTEGER record must contain a field".into());
            }
            Ok(Some(result))
        }
        _ => Ok(None),
    }
}

/// Source-checked record layout metadata, independent of any compiler IR.
/// This first producer covers the bridge's existing INTEGER-only records.
/// Names are for diagnostics; declaration IDs are used for field selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckedRecordLayout {
    pub declaration: String,
    pub base: Option<String>,
    pub fields: Vec<CheckedRecordField>,
    pub size_bytes: u32,
    pub alignment_bytes: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckedRecordField {
    pub declaration: String,
    pub name: String,
    pub ty: CheckedRecordFieldType,
    pub offset_bytes: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckedRecordFieldType {
    Integer,
    Byte,
    Record(String),
}

/// The source-checked object declaration and its exact nominal record.
/// Procedure-local objects use `function=Some(procedure_name)`; module
/// globals use `None`. This is metadata, not an ABI-visible record type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckedRecordObject {
    pub function: Option<String>,
    pub name: String,
    pub record: String,
}

/// Existing checked Module, source export names, record declarations and
/// per-object identities. Named to keep the public API readable.
pub type CheckedRecordProjection = (
    Module, Vec<String>, Vec<CheckedRecordLayout>, Vec<CheckedRecordObject>,
);

/// Natural byte layout of the checked private INTEGER/BYTE subset.
fn align_checked_record(bytes:u32,alignment:u32)->Result<u32,String> {
    if !matches!(alignment,1|4) {
        return Err("unsupported private record scalar alignment".into());
    }
    bytes.checked_add(alignment-1)
        .map(|n|n&!(alignment-1))
        .ok_or_else(||"checked record byte layout overflow".into())
}

/// Build metadata only from the already semantically checked module AST.
/// Each source record declaration is emitted exactly once; nested inline
/// records receive stable path-scoped identities and named aliases retain
/// their original nominal identity. This does not admit exported ABI/VAR/FP.
fn checked_record_layout(
    ty: &TypeExpr,
    identity: &str,
    named: &HashMap<String, String>,
    layouts: &mut Vec<CheckedRecordLayout>,
) -> Result<Option<String>, String> {
    match ty {
        TypeExpr::Named(q, _) if q.len() == 1 => Ok(named.get(&q[0]).cloned()),
        TypeExpr::Record { base, fields, .. } => {
            let parent = if let Some(q) = base {
                if q.len() != 1 {
                    return Err("qualified record base needs import support".into());
                }
                Some(named.get(&q[0])
                    .ok_or("undefined checked base record identity")?.clone())
            } else { None };
            let (mut extent,mut alignment)=if let Some(base)=&parent {
                let b=layouts.iter().find(|r|&r.declaration==base)
                    .ok_or("checked base record layout missing")?;
                (b.size_bytes,b.alignment_bytes)
            } else {(0,1)};
            let mut out = Vec::new();
            for field in fields {
                for (name, _) in &field.names {
                    let child_id = format!("{identity}.{name}");
                    let child = checked_record_layout(&field.ty, &child_id, named, layouts)?;
                    let kind = match (&field.ty, child) {
                        (TypeExpr::Named(q, _), None) if q.as_slice() == ["INTEGER"] =>
                            CheckedRecordFieldType::Integer,
                        (TypeExpr::Named(q, _), None) if q.as_slice() == ["BYTE"] =>
                            CheckedRecordFieldType::Byte,
                        (_, Some(id)) => CheckedRecordFieldType::Record(id),
                        _ => return Err("checked record metadata needs INTEGER or record field".into()),
                    };
                    let (bytes,align) = match &kind {
                        CheckedRecordFieldType::Integer => (4,4),
                        CheckedRecordFieldType::Byte => (1,1),
                        CheckedRecordFieldType::Record(id) => {
                            let nested=layouts.iter().find(|r|&r.declaration==id)
                                .ok_or("checked nested record layout missing")?;
                            (nested.size_bytes,nested.alignment_bytes)
                        },
                    };
                    // All recorded byte offsets come from the semantically
                    // checked field sequence and natural target alignment.
                    extent=align_checked_record(extent,align)?;
                    alignment=alignment.max(align);
                    // Field declaration identities are never derived from
                    // field *names* alone: source spans disambiguate declarations.
                    let declaration = format!("{}:{}:{}", field.span.lo, field.span.hi, name);
                    out.push(CheckedRecordField {
                        declaration, name: name.clone(), ty: kind, offset_bytes: extent,
                    });
                    extent = extent.checked_add(bytes)
                        .ok_or("checked record layout size overflow")?;
                    if extent > 4096 {
                        return Err("native INTEGER record exceeds 1024 words".into());
                    }
                }
            }
            if extent == 0 {
                return Err("empty checked record layout unsupported".into());
            }
            let total=align_checked_record(extent,alignment)?;
            if total>4096 {
                return Err("checked record object exceeds maximum byte size".into());
            }
            layouts.push(CheckedRecordLayout {
                declaration: identity.into(), base: parent,
                fields: out, size_bytes: total, alignment_bytes: alignment,
            });
            Ok(Some(identity.into()))
        }
        _ => Ok(None),
    }
}

/// The source record owns exact byte offsets; an INTEGER-only source
/// retains the old word-index bridge for differential tests. Mixed records
/// use explicit byte-addressable leaf operations, never name-guessed slots.
#[derive(Clone,Debug)]
struct RecordShape {
    fields: Vec<String>,
    bytes: Vec<(u32,u8)>,
    has_byte: bool,
    words: u32,
}

fn record_shape(
    identity:&str, layouts:&[CheckedRecordLayout], legacy_fields:&[String],
) -> Result<RecordShape,String> {
    fn visit(
        id:&str,prefix:&str,offset:u32,layouts:&[CheckedRecordLayout],
        out:&mut Vec<(String,u32,u8)>,
    )->Result<(),String> {
        let record=layouts.iter().find(|r|r.declaration==id)
            .ok_or("checked record object refers to missing record declaration")?;
        if let Some(base)=&record.base {
            visit(base,prefix,offset,layouts,out)?;
        }
        for field in &record.fields {
            let field_offset=offset.checked_add(field.offset_bytes)
                .ok_or("record field byte offset overflow")?;
            let path=format!("{prefix}{}",field.name);
            match &field.ty {
                CheckedRecordFieldType::Integer=>out.push((path,field_offset,32)),
                CheckedRecordFieldType::Byte=>out.push((path,field_offset,8)),
                CheckedRecordFieldType::Record(child)=>{
                    visit(child,&format!("{path}."),field_offset,layouts,out)?;
                },
            }
        }
        Ok(())
    }
    let layout=layouts.iter().find(|r|r.declaration==identity)
        .ok_or("record object layout missing")?;
    // First executable mixed slice reuses the native 4-byte frame-array
    // root, including its existing zeroing and checked provenance.
    // Pure BYTE objects need exact-sized byte roots and stay fail-closed.
    if layout.alignment_bytes!=4 || layout.size_bytes%4!=0 {
        return Err("BYTE-only record storage requires native byte allocation".into());
    }
    let mut leaves=Vec::new();
    visit(identity,"",0,layouts,&mut leaves)?;
    if leaves.iter().map(|x|x.0.as_str()).ne(legacy_fields.iter().map(String::as_str)) {
        return Err("record layout leaf order disagrees with checked field projection".into());
    }
    let size=layout.size_bytes;
    for (_,offset,bits) in &leaves {
        if offset.checked_add(u32::from(*bits)/8).is_none_or(|end|end>size) {
            return Err("record leaf exceeds verified byte object extent".into());
        }
    }
    let has_byte=leaves.iter().any(|x|x.2==8);
    Ok(RecordShape {
        fields:legacy_fields.to_vec(),
        bytes:leaves.iter().map(|x|(x.1,x.2)).collect(),
        has_byte,words:size/4,
    })
}

type RecordObjects = HashMap<String,(usize,RecordShape)>;
fn record_field(records:&RecordObjects, base:&str, field:&str)->Result<Expr,String> {
    let (object,shape)=records.get(base)
        .ok_or_else(||format!("unsupported record object {base}"))?;
    let offset=shape.fields.iter().position(|f|f==field)
        .ok_or_else(||format!("unsupported record field {base}.{field}"))?;
    if shape.has_byte {
        let (byte_offset,bits)=shape.bytes[offset];
        Ok(Expr::RecordField {
            array:*object,byte_offset,bits,
        })
    } else {
        Ok(Expr::ArrayElement {
            array:*object,index:Box::new(Expr::Integer(offset as i32)),
        })
    }
}

fn record_store(
    records:&RecordObjects, base:&str,field:&str,value:Expr,
)->Result<Stmt,String> {
    Ok(match record_field(records,base,field)? {
        Expr::ArrayElement {array,index}=>Stmt::AssignArray {
            array,index:*index,value,
        },
        Expr::RecordField {array,byte_offset,bits}=>Stmt::AssignRecordField {
            array,byte_offset,bits,value,
        },
        _=>return Err("checked record selector must refer to a scalar field".into()),
    })
}

/// Only direct checked record objects and field chains are supported:
/// pointer dereferences, module-qualified roots and indexing stay rejected.
fn record_selector_path(d:&syntax::ast::Designator)->Option<String> {
    if d.base.len()!=1 || !d.sels.iter().all(|s|
        matches!(s,syntax::ast::Selector::Field(_))) {
        return None;
    }
    Some(d.sels.iter().map(|s|match s {
        syntax::ast::Selector::Field(name)=>name.as_str(),
        _=>unreachable!("guarded field selector"),
    }).collect::<Vec<_>>().join("."))
}

/// Return the contiguous leaf-word interval and relative leaf names for a
/// whole record or a selected subrecord. A scalar field has no descendants.
fn record_region(records:&RecordObjects, root:&str, path:&str)
    -> Option<(usize,Vec<(usize,String)>)>
{
    let (array,shape)=records.get(root)?;
    if path.is_empty() {
        return Some((*array,shape.fields.iter().enumerate().map(|(i,name)|
            (i,name.clone())).collect()));
    }
    let prefix=format!("{path}.");
    let words=shape.fields.iter().enumerate().filter_map(|(offset,name)|
        name.strip_prefix(&prefix).map(|suffix|(offset,suffix.to_owned())))
        .collect::<Vec<_>>();
    (!words.is_empty()).then_some((*array,words))
}

/// A whole record or selected subrecord assignment uses only frontend-checked
/// direct objects, with identical leaf paths or a derived-to-base prefix.
/// Contiguous field copies are ordered like memmove for same-object aliasing.
fn lower_record_copy(target:&syntax::ast::Designator, value:&OberonExpr,
                     records:&RecordObjects)->Result<Option<Vec<Stmt>>,String> {
    let Some(dst_path)=record_selector_path(target) else {return Ok(None)};
    let Some((dest,dst_words))=record_region(records,&target.base[0],&dst_path) else {
        return Ok(None)
    };
    let ExprKind::Designator(src)=&value.kind else {
        return Err("record assignment requires direct record designator".into())
    };
    let src_path=record_selector_path(src)
        .ok_or("unsupported selected record assignment source")?;
    let (source,src_words)=record_region(records,&src.base[0],&src_path)
        .ok_or("record assignment source must be a supported record object")?;
    // A mixed-width copy must include every source byte/padding and alias
    // effect. The old leaf-word copy is not an acceptable substitute.
    if records.get(&target.base[0]).is_some_and(|(_,s)|s.has_byte) ||
       records.get(&src.base[0]).is_some_and(|(_,s)|s.has_byte) {
        return Err("mixed-width record copy requires byte-range legalization".into());
    }
    if src_words.len()<dst_words.len() ||
       !src_words.iter().zip(&dst_words).all(|(from,to)|from.1==to.1) {
        return Err("record assignment requires compatible source prefix".into())
    }
    let backward=source==dest && dst_words[0].0>src_words[0].0;
    let mut offsets=(0..dst_words.len()).collect::<Vec<_>>();
    if backward {offsets.reverse();}
    Ok(Some(offsets.into_iter().map(|i|Stmt::AssignArray {
        array:dest,index:Expr::Integer(dst_words[i].0 as i32),
        value:Expr::ArrayElement {
            array:source,index:Box::new(Expr::Integer(src_words[i].0 as i32)),
        },
    }).collect()))
}

fn lower_call(d:&syntax::ast::Designator,
              names:&HashMap<&str,usize>,
              arrays:&HashMap<&str,usize>,
              records:&RecordObjects,
              functions:&HashMap<String,usize>)->Result<Expr,String> {
    if d.base.len()!=1 || d.sels.len()!=1 {
        return Err("unsupported qualified or selected function call".into());
    }
    let symbol=&d.base[0];
    let expected=*functions.get(symbol)
        .ok_or_else(||format!("unsupported call to non-local function {symbol}"))?;
    let raw_args=match &d.sels[0] {
        syntax::ast::Selector::Call(args)=>args.as_slice(),
        syntax::ast::Selector::Args(args)=>args.as_deref().unwrap_or(&[]),
        _=>return Err("expected direct function call".into()),
    };
    if raw_args.len()!=expected {
        return Err(format!("{symbol}: call argument count mismatch"));
    }
    let mut args=Vec::with_capacity(raw_args.len());
    for arg in raw_args {
        args.push(lower_expr(arg,names,arrays,records,functions)?);
    }
    Ok(Expr::Call{name:symbol.clone(),args})
}

fn lower_expr(expr: &OberonExpr, names: &HashMap<&str, usize>,
              arrays: &HashMap<&str, usize>,
              records: &RecordObjects,
              functions:&HashMap<String,usize>) -> Result<Expr, String> {
    match &expr.kind {
        ExprKind::Designator(d) if d.base.len()==1 && d.sels.len()==1
            && matches!(d.sels[0],syntax::ast::Selector::Call(_)|
                        syntax::ast::Selector::Args(_)) => {
            lower_call(d,names,arrays,records,functions)
        }
        ExprKind::Int(value) => Ok(Expr::Integer(*value as i32)),
        ExprKind::Bool(value) => Ok(Expr::Boolean(*value)),
        ExprKind::Designator(d) if d.sels.is_empty() && d.base.len() == 1 => {
            names.get(d.base[0].as_str()).copied().map(Expr::Variable)
                .ok_or_else(|| format!("unsupported variable or constant: {}", d.base[0]))
        }
        ExprKind::Designator(d) if d.base.len()==1 && !d.sels.is_empty() &&
            d.sels.iter().all(|sel|matches!(sel,syntax::ast::Selector::Field(_))) => {
            let fields=d.sels.iter().map(|sel|match sel {
                syntax::ast::Selector::Field(name)=>name.as_str(),
                _=>unreachable!("guarded field selectors"),
            }).collect::<Vec<_>>().join(".");
            record_field(records,&d.base[0],&fields)
        }
        ExprKind::Designator(d) if d.base.len()==1 && d.sels.len()==1 => {
            match &d.sels[0] {
                syntax::ast::Selector::Field(field) => {
                    record_field(records,&d.base[0],field)
                }
                syntax::ast::Selector::Index(indices) if indices.len()==1 => {
                    let array=*arrays.get(d.base[0].as_str())
                        .ok_or_else(||format!("not a supported local array: {}",d.base[0]))?;
                    Ok(Expr::ArrayElement {
                        array,index:Box::new(lower_expr(&indices[0],names, arrays, records, functions)?),
                    })
                }
                _=>Err(format!("unsupported array selector at {}",d.span.lo)),
            }
        },
        ExprKind::Binary(op, a, b) => {
            let op = match op {
                BinOp::Add => Op::Add,
                BinOp::Sub => Op::Sub,
                BinOp::Mul => Op::Mul,
                BinOp::Eq => Op::Eq,
                BinOp::Ne => Op::Ne,
                BinOp::Lt => Op::Lt,
                BinOp::Le => Op::Le,
                BinOp::Gt => Op::Gt,
                BinOp::Ge => Op::Ge,
                _ => return Err(format!("unsupported binary expression: {op:?}")),
            };
            Ok(Expr::Binary(op, Box::new(lower_expr(a, names, arrays, records, functions)?),
                            Box::new(lower_expr(b, names, arrays, records, functions)?)))
        }
        ExprKind::Unary(UnOp::Plus, a) => lower_expr(a, names, arrays, records, functions),
        ExprKind::Unary(UnOp::Neg, a) =>
            Ok(Expr::Binary(Op::Sub, Box::new(Expr::Integer(0)),
                            Box::new(lower_expr(a, names, arrays, records, functions)?))),
        _ => Err(format!("unsupported expression at {}", expr.span.lo)),
    }
}

fn lower_stmts(stmts: &[OberonStmt], names: &HashMap<&str, usize>,
              arrays: &HashMap<&str, usize>,
              records: &RecordObjects,
              functions:&HashMap<String,usize>) -> Result<Vec<Stmt>, String>
{
    let mut out = Vec::new();
    for s in stmts {
        match &s.kind {
            StmtKind::Call(d)=>out.push(Stmt::Call(
                lower_call(d,names,arrays,records,functions)?)),
            StmtKind::Assign { target, value } if target.sels.is_empty() &&
                                                   target.base.len() == 1 => {
                if let Some(copies)=lower_record_copy(target,value,records)? {
                    out.extend(copies);
                } else {
                    let variable=names.get(target.base[0].as_str()).copied()
                        .ok_or_else(||format!("unknown local or parameter: {}",target.base[0]))?;
                    out.push(Stmt::Assign {variable,value:
                        lower_expr(value,names,arrays,records,functions)?});
                }
            }
            StmtKind::Assign { target, value } if target.base.len()==1 &&
                !target.sels.is_empty() &&
                target.sels.iter().all(|sel|matches!(sel,syntax::ast::Selector::Field(_))) => {
                if let Some(copies)=lower_record_copy(target,value,records)? {
                    out.extend(copies);
                } else {
                    let fields=record_selector_path(target)
                        .ok_or("unsupported record selector")?;
                    out.push(record_store(records,&target.base[0],&fields,
                        lower_expr(value,names,arrays,records,functions)?)?);
                }
            }
            StmtKind::Assign { target, value } if target.base.len()==1 &&
                                                    target.sels.len()==1 => {
                match &target.sels[0] {
                    syntax::ast::Selector::Field(field) => {
                        out.push(record_store(records,&target.base[0],field,
                            lower_expr(value,names,arrays,records,functions)?)?);
                    }
                    syntax::ast::Selector::Index(indices) if indices.len()==1 => {
                        let array=*arrays.get(target.base[0].as_str())
                            .ok_or_else(||format!("not a supported local array: {}",target.base[0]))?;
                        out.push(Stmt::AssignArray {
                            array,
                            index:lower_expr(&indices[0],names, arrays, records, functions)?,
                            value:lower_expr(value,names, arrays, records, functions)?,
                        });
                    }
                    _=>return Err(format!("unsupported array assignment at {}",s.span.lo)),
                }
            }
            StmtKind::Return(Some(e)) =>
                out.push(Stmt::Return(lower_expr(e, names, arrays, records, functions)?)),
            StmtKind::If { arms, els } => {
                if arms.is_empty() { return Err("IF with no arm".into()); }
                let mut tail = match els {
                    Some(stmts) => lower_stmts(stmts, names, arrays, records, functions)?,
                    None => Vec::new(),
                };
                // ELSIF is lowered into a nested IF in the previous else arm.
                for (condition, body) in arms.iter().rev() {
                    tail = vec![Stmt::If {
                        condition: lower_expr(condition, names, arrays, records, functions)?,
                        then_body: lower_stmts(body, names, arrays, records, functions)?,
                        else_body: tail,
                    }];
                }
                out.extend(tail);
            }
            StmtKind::While { arms } if arms.len() == 1 => {
                let (condition, body) = &arms[0];
                out.push(Stmt::While {
                    condition: lower_expr(condition, names, arrays, records, functions)?,
                    body: lower_stmts(body, names, arrays, records, functions)?,
                });
            }
            _ => return Err(format!("unsupported Oberon statement at {}", s.span.lo)),
        }
    }
    Ok(out)
}

/// Parse/type-check in process, then export a small checked procedure subset.
/// Same-module integer calls to declared functions and recursive calls are
/// projected only when the callee signature is supported (up to sixteen value
/// INTEGER parameters). Module imports and external/runtime references are
/// not implemented.
pub fn lower_module(source: &str) -> Result<Module, String> {
    lower_module_with_exports(source).map(|(module, _)| module)
}

/// Preserve checked source-level procedure visibility without exposing
/// parser AST types or forcing downstream native backends to reparse source.
/// Exported Oberon procedures carry `*`; omitted `*` means module-private.
/// The older `lower_module` API remains unchanged for existing consumers.
pub fn lower_module_with_exports(source: &str) -> Result<(Module, Vec<String>), String> {
    lower_module_with_metadata(source).map(|(module, exports, _)| (module, exports))
}

/// One semantic-check/parse pass for native consumers that also need checked
/// nominal record/field identity and byte layouts. No raw AST is exposed.
pub fn lower_module_with_metadata(source: &str)
    -> Result<(Module, Vec<String>, Vec<CheckedRecordLayout>), String> {
    lower_module_with_record_objects(source)
        .map(|(module, exports, layouts, _)| (module, exports, layouts))
}

/// Parse and check once, preserving the declaration identity of both
/// supported records and the module/procedure objects that instantiate them.
/// Existing front-ir output remains ABI- and layout-compatible.
pub fn lower_module_with_record_objects(source: &str)
    -> Result<CheckedRecordProjection, String> {
    let (checked, diagnostics) = sema::check_module_recovering(source, &[]);
    if let Some(d) = diagnostics.iter().find(|d| d.is_error()) {
        return Err(format!("Oberon semantic or syntax error: {d:?}"));
    }
    let module = checked.ok_or("Oberon parser did not produce a module")?;
    let exported_procedures = module.decls.procs.iter()
        .filter(|proc| proc.export)
        .map(|proc| proc.name.clone())
        .collect::<Vec<_>>();
    if !module.imports.is_empty() || !module.decls.consts.is_empty() {
        return Err("module imports or constants are not yet supported".into());
    }
    let mut record_types = HashMap::<String, Vec<String>>::new();
    let mut record_nominals = HashMap::<String, String>::new();
    let mut checked_layouts = Vec::<CheckedRecordLayout>::new();
    let mut checked_objects = Vec::<CheckedRecordObject>::new();
    for decl in &module.decls.types {
        if decl.export {
            return Err("exported record types require a native ABI layout".into());
        }
        let fields = record_fields(&decl.ty, &record_types)?
            .ok_or_else(|| format!("unsupported type declaration: {}", decl.name))?;
        let declaration = format!("{}.{}", module.name, decl.name);
        let nominal = checked_record_layout(
            &decl.ty, &declaration, &record_nominals, &mut checked_layouts,
        )?.ok_or("unsupported source-checked record type")?;
        record_nominals.insert(decl.name.clone(), nominal);
        record_types.insert(decl.name.clone(), fields);
    }
    let mut globals = Vec::<Global>::new();
    let mut global_arrays = Vec::<ArrayGlobal>::new();
    let mut module_record_fields=HashMap::<String,RecordShape>::new();
    for decl in &module.decls.vars {
        if let Some(fields) = record_fields(&decl.ty, &record_types)? {
            let first = decl.names.first().ok_or("record variable declaration has no names")?;
            let identity = format!("{}.global.{}", module.name, first.0);
            let record = checked_record_layout(
                &decl.ty, &identity, &record_nominals, &mut checked_layouts,
            )?.ok_or("checked global record identity is missing")?;
            let shape=record_shape(&record,&checked_layouts,&fields)?;
            for (name, exported) in &decl.names {
                if *exported {
                    return Err("exported record objects require a native ABI layout".into());
                }
                global_arrays.push(ArrayGlobal {
                    name:name.clone(),length:shape.words,exported:false,
                });
                module_record_fields.insert(name.clone(),shape.clone());
                checked_objects.push(CheckedRecordObject {
                    function:None, name:name.clone(), record:record.clone(),
                });
            }
            continue;
        }
        match &decl.ty {
            TypeExpr::Named(q, _) if q.as_slice()==["INTEGER"] => {
                for (name, exported) in &decl.names {
                    globals.push(Global { name: name.clone(), exported: *exported });
                }
            }
            TypeExpr::Array { dims, elem, .. } if dims.len()==1 &&
                matches!(elem.as_ref(),TypeExpr::Named(q,_) if q.as_slice()==["INTEGER"]) => {
                let ExprKind::Int(length)=&dims[0].kind else {
                    return Err("module array length must be an INTEGER literal".into());
                };
                if *length == 0 || *length > 1024 {
                    return Err("module array length must be within 1..=1024".into());
                }
                for (name, exported) in &decl.names {
                    global_arrays.push(ArrayGlobal {
                        name: name.clone(), length: *length, exported: *exported,
                    });
                }
            }
            _ => return Err("only INTEGER globals and fixed INTEGER arrays are supported".into()),
        }
    }

    // Collect the checked signatures before lowering procedure bodies. The
    // Oberon semantic checker already enforces source declaration order;
    // this map does not authorize forward references.
    let mut signatures=HashMap::<String,usize>::new();
    for proc in &module.decls.procs {
        let count=proc.params.iter().map(|section|section.names.len()).sum();
        if signatures.insert(proc.name.clone(),count).is_some() {
            return Err(format!("duplicate procedure {}",proc.name));
        }
    }
    let mut functions = Vec::new();
    for proc in &module.decls.procs {
        if !matches!(proc.result.as_deref(), Some([t]) if t == "INTEGER") {
            return Err(format!("{}: only INTEGER-returning functions are supported", proc.name));
        }
        if !proc.decls.procs.is_empty() || !proc.decls.consts.is_empty() ||
           !proc.decls.types.is_empty() {
            return Err(format!("{}: nested procedures, constants or types unsupported", proc.name));
        }
        let mut parameters = Vec::new();
        for section in &proc.params {
            if section.by_ref || section.ty.open != 0 ||
               section.ty.base.as_slice() != ["INTEGER"] {
                return Err(format!("{}: only value INTEGER parameters are supported", proc.name));
            }
            for (name, _) in &section.names { parameters.push(name.clone()); }
        }
        let mut locals = Vec::new();
        let mut fixed_arrays = Vec::new();
        let mut local_record_fields=HashMap::<String,RecordShape>::new();
        for decl in &proc.decls.vars {
            if let Some(fields) = record_fields(&decl.ty, &record_types)? {
                let first = decl.names.first().ok_or("local record declaration has no names")?;
                let identity = format!("{}.{}.local.{}", module.name, proc.name, first.0);
                let record = checked_record_layout(
                    &decl.ty, &identity, &record_nominals, &mut checked_layouts,
                )?.ok_or("checked local record identity is missing")?;
                let shape=record_shape(&record,&checked_layouts,&fields)?;
                for (name, _) in &decl.names {
                    fixed_arrays.push(ArrayLocal {
                        name:name.clone(),length:shape.words,
                    });
                    local_record_fields.insert(name.clone(),shape.clone());
                    checked_objects.push(CheckedRecordObject {
                        function:Some(proc.name.clone()), name:name.clone(),
                        record:record.clone(),
                    });
                }
                continue;
            }
            match &decl.ty {
                TypeExpr::Named(q, _) if q.as_slice()==["INTEGER"] => {
                    for (name,_) in &decl.names {locals.push(name.clone());}
                }
                TypeExpr::Array{dims,elem,..} if dims.len()==1 &&
                    matches!(elem.as_ref(),TypeExpr::Named(q,_) if q.as_slice()==["INTEGER"]) => {
                    // Do not invent a constant evaluator or assume a stride
                    // for a non-integer/multidimensional element type.
                    let ExprKind::Int(length)=&dims[0].kind else {
                        return Err(format!("{}: array length must be an INTEGER literal",proc.name));
                    };
                    if *length==0 || *length>1024 {
                        return Err(format!("{}: array length must be within 1..=1024",proc.name));
                    }
                    for (name,_) in &decl.names {
                        fixed_arrays.push(ArrayLocal{name:name.clone(),length:*length});
                    }
                }
                _=>return Err(format!("{}: only INTEGER locals and fixed INTEGER arrays supported",proc.name)),
            }
        }
        // Modules and procedures use separate scopes: a local or parameter
        // shadows a global of the same name. Global indexes begin after
        // the complete local scalar index range.
        let mut names: HashMap<&str, usize> = globals.iter().enumerate()
            .map(|(i, g)| (g.name.as_str(), parameters.len() + locals.len() + i))
            .collect();
        for (i, name) in parameters.iter().chain(locals.iter()).enumerate() {
            names.insert(name.as_str(), i);
        }
        // Local array names shadow module array names; global arrays
        // follow local arrays in the checked bridge's array index space.
        let mut arrays:HashMap<&str,usize>=global_arrays.iter().enumerate()
            .map(|(i,a)|(a.name.as_str(),fixed_arrays.len()+i)).collect();
        for (i, array) in fixed_arrays.iter().enumerate() {
            arrays.insert(array.name.as_str(), i);
        }
        let mut records:RecordObjects=global_arrays.iter().enumerate()
            .filter_map(|(i,a)|module_record_fields.get(&a.name).map(|shape|
                (a.name.clone(),(fixed_arrays.len()+i,shape.clone()))))
            .collect();
        // Locals and parameters shadow module record names.
        for name in parameters.iter().chain(locals.iter())
            .chain(fixed_arrays.iter().map(|a|&a.name)) { records.remove(name); }
        for (i,a) in fixed_arrays.iter().enumerate() {
            if let Some(shape)=local_record_fields.get(&a.name) {
                records.insert(a.name.clone(),(i,shape.clone()));
            }
        }
        let body=lower_stmts(&proc.body,&names,&arrays,&records,&signatures)?;
        // An absent return is rejected rather than relying on Oberon's
        // zero-valued return accumulator (not modeled by this narrow bridge).
        if !contains_return(&body) {
            return Err(format!("{}: function must return a value", proc.name));
        }
        functions.push(Function { name: proc.name.clone(), parameters, locals,
                                  arrays:fixed_arrays,
                                  global_arrays:global_arrays.clone(),
                                  globals: globals.iter().map(|g| g.name.clone()).collect(),
                                  body });
    }
    let initializer = if module.body.is_empty() {
        None
    } else {
        // Module code has no parameters or function-local variables. Only
        // verified module globals are legal roots for assignments/indexing.
        let names: HashMap<&str, usize> = globals.iter().enumerate()
            .map(|(i, item)| (item.name.as_str(), i)).collect();
        let arrays: HashMap<&str, usize> = global_arrays.iter().enumerate()
            .map(|(i, item)| (item.name.as_str(), i)).collect();
        let records:RecordObjects=global_arrays.iter().enumerate()
            .filter_map(|(i,a)|module_record_fields.get(&a.name).map(|shape|
                (a.name.clone(),(i,shape.clone()))))
            .collect();
        let body=lower_stmts(&module.body,&names,&arrays,&records,&signatures)?;
        if contains_return(&body) {
            return Err("module initialization cannot return a value".into());
        }
        Some(Function {
            name: "__module_init".into(),
            parameters: Vec::new(), locals: Vec::new(), arrays: Vec::new(),
            global_arrays: global_arrays.clone(),
            globals: globals.iter().map(|g| g.name.clone()).collect(),
            body,
        })
    };
    if functions.is_empty() && globals.is_empty() && global_arrays.is_empty()
        && initializer.is_none() {
        return Err("module has no supported procedures, globals or initialization".into());
    }
    Ok((
        Module { name: module.name, globals, global_arrays, functions, initializer },
        exported_procedures,
        checked_layouts,
        checked_objects,
    ))
}

fn contains_return(body: &[Stmt]) -> bool {
    body.iter().any(|s| match s {
        Stmt::Return(_) => true,
        Stmt::If { then_body, else_body, .. } =>
            contains_return(then_body) || contains_return(else_body),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_private_integer_record_fields_without_abi_or_address_exposure() {
        let source = "MODULE Records;
            TYPE Pair = RECORD left, right: INTEGER END;
            VAR pair: Pair; tally: INTEGER;
            PROCEDURE Sum*(n: INTEGER): INTEGER;
            VAR tmp: Pair; sum: INTEGER;
            BEGIN
                tmp.left := n;
                tmp.right := pair.left;
                sum := tmp.left + tmp.right;
                RETURN sum
            END Sum;
            BEGIN
                pair.left := 7;
                pair.right := pair.left + 2;
                tally := pair.right
            END Records.";
        let module = lower_module(source).unwrap();
        assert_eq!(module.globals.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(),
                   ["tally"]);
        assert_eq!(module.global_arrays,
                   [ArrayGlobal{name:"pair".into(),length:2,exported:false}]);
        let f = &module.functions[0];
        assert_eq!(f.locals, ["sum"]);
        assert_eq!(f.arrays, [ArrayLocal{name:"tmp".into(),length:2}]);
        assert!(matches!(&f.body[0], Stmt::AssignArray {
            array:0,index:Expr::Integer(0),value:Expr::Variable(0),
        }));
        assert!(matches!(&f.body[1], Stmt::AssignArray {
            array:0,index:Expr::Integer(1),
            value:Expr::ArrayElement {array:1,index,..}
        } if matches!(index.as_ref(), Expr::Integer(0))));
        assert!(matches!(&module.initializer.as_ref().unwrap().body[0],
                         Stmt::AssignArray {array:0,index:Expr::Integer(0),
                             value:Expr::Integer(7)}));
    }

    #[test]
    fn contiguous_record_assignment_copies_all_fields() {
        let src="MODULE Copy;
            TYPE Triple = RECORD a,b,c: INTEGER END;
            VAR x,y: Triple;
            PROCEDURE F*(n: INTEGER): INTEGER;
            VAR p,q: Triple;
            BEGIN
                p.a:=n; p.b:=n+1; p.c:=n+2;
                q:=p;
                x:=q;
                y:=x;
                RETURN y.a+y.b+y.c
            END F;
            END Copy.";
        let m=lower_module(src).unwrap();
        assert_eq!(m.global_arrays.len(),2);
        assert!(m.globals.is_empty());
        let f=&m.functions[0];
        assert_eq!(f.arrays.len(),2);
        assert_eq!(f.arrays[0].length,3);
        assert_eq!(f.body.iter().filter(|s|matches!(s,Stmt::AssignArray{..})).count(),12);
        assert!(matches!(f.body.last(),Some(Stmt::Return(_))));
    }

    #[test]
    fn inherited_integer_fields_keep_prefix_offsets_and_base_copies() {
        let source="MODULE Ext;
            TYPE Base = RECORD a,b: INTEGER END;
                 Derived = RECORD (Base) c: INTEGER END;
            VAR b: Base; d: Derived;
            PROCEDURE F*(n: INTEGER): INTEGER;
            VAR q: Derived;
            BEGIN
                q.a:=n; q.b:=n+1; q.c:=n+2;
                d:=q;
                b:=d;
                RETURN b.a+b.b+d.c
            END F;
            END Ext.";
        let m=lower_module(source).unwrap();
        assert_eq!(m.global_arrays.len(),2);
        assert_eq!(m.global_arrays[0].length,2);
        assert_eq!(m.global_arrays[1].length,3);
        let f=&m.functions[0];
        assert_eq!(f.arrays,[ArrayLocal{name:"q".into(),length:3}]);
        assert!(matches!(&f.body[0],Stmt::AssignArray {
            array:0,index:Expr::Integer(0),..
        }));
        // Inherited field b keeps offset 1; derived-only c is at offset 2.
        assert!(matches!(&f.body[1],Stmt::AssignArray {
            array:0,index:Expr::Integer(1),..
        }));
        assert!(matches!(&f.body[2],Stmt::AssignArray {
            array:0,index:Expr::Integer(2),..
        }));
        // 3 source stores + 3 derived copies + only 2 base-prefix copies.
        assert_eq!(f.body.iter().filter(|stmt|matches!(stmt,Stmt::AssignArray{..})).count(),8);
        let invalid="MODULE Bad;
            TYPE Base = RECORD x: INTEGER END;
                 Child = RECORD (Base) y: INTEGER END;
            VAR base: Base; child: Child;
            BEGIN child := base END Bad.";
        assert!(lower_module(invalid).is_err(),"base-to-derived assignment must reject");
    }

    #[test]
    fn nested_integer_records_are_contiguous_and_have_stable_field_paths() {
        let source="MODULE Nested;
            TYPE Leaf = RECORD a,b: INTEGER END;
                 Mid = RECORD one: Leaf; middle: INTEGER; two: Leaf END;
                 Top = RECORD head: Mid; last: INTEGER END;
            VAR global: Top;
            PROCEDURE Work*(x: INTEGER): INTEGER;
            VAR local: Top;
            BEGIN
                local.head.one.a:=x;
                local.head.one.b:=x+1;
                local.head.middle:=x+2;
                local.head.two.a:=x+3;
                local.head.two.b:=x+4;
                local.last:=x+5;
                global.head.one.a:=local.head.one.a;
                global.head.two.b:=local.head.two.b;
                RETURN global.head.one.a+global.head.two.b
            END Work;
            END Nested.";
        let m=lower_module(source).unwrap();
        assert_eq!(m.global_arrays,
            [ArrayGlobal{name:"global".into(),length:6,exported:false}]);
        let f=&m.functions[0];
        assert_eq!(f.arrays,[ArrayLocal{name:"local".into(),length:6}]);
        for offset in 0..6 {
            assert!(matches!(&f.body[offset],
                Stmt::AssignArray{array:0,index:Expr::Integer(i),..}
                if *i==offset as i32));
        }
        assert!(matches!(&f.body[6],
            Stmt::AssignArray {array:1,index:Expr::Integer(0),
                value:Expr::ArrayElement{array:0,..}}));
        assert!(matches!(&f.body[7],
            Stmt::AssignArray {array:1,index:Expr::Integer(4),
                value:Expr::ArrayElement{array:0,..}}));
    }

    #[test]
    fn copies_nested_records_between_roots_and_siblings() {
        let src="MODULE NestedCopy;
            TYPE Pair = RECORD a,b: INTEGER END;
                 Tree = RECORD left,right: Pair; tail: INTEGER END;
            VAR global: Tree;
            PROCEDURE F*(n: INTEGER): INTEGER;
            VAR tree: Tree; separate: Pair;
            BEGIN
                separate.a:=n; separate.b:=n+1;
                tree.left:=separate;
                tree.right:=tree.left;
                tree.tail:=n+2;
                global:=tree;
                RETURN global.left.a+global.right.b+global.tail
            END F;
            END NestedCopy.";
        let module=lower_module(src).unwrap();
        assert_eq!(module.global_arrays[0].length,5);
        let f=&module.functions[0];
        assert_eq!(f.arrays[0].length,5);
        assert_eq!(f.arrays[1].length,2);
        assert_eq!(f.body.iter().filter(|s|matches!(s,Stmt::AssignArray{..})).count(),12);
        // Read from separate[0], write into tree[0].
        assert!(matches!(&f.body[2],Stmt::AssignArray {
            array:0,index:Expr::Integer(0),
            value:Expr::ArrayElement{array:1,index,..}
        } if matches!(index.as_ref(),Expr::Integer(0))));
        // For a same-object copy to a higher address the memmove-safe
        // direction is right[1] := left[1], then right[0] := left[0].
        assert!(matches!(&f.body[4],Stmt::AssignArray {
            array:0,index:Expr::Integer(3),
            value:Expr::ArrayElement{array:0,index,..}
        } if matches!(index.as_ref(),Expr::Integer(1))));
        assert!(matches!(&f.body[5],Stmt::AssignArray {
            array:0,index:Expr::Integer(2),
            value:Expr::ArrayElement{array:0,index,..}
        } if matches!(index.as_ref(),Expr::Integer(0))));
    }

    #[test]
    fn nested_record_field_paths_reject_scalar_intermediates() {
        let invalid="MODULE Bad;
            TYPE R = RECORD a: INTEGER END;
            VAR r: R;
            PROCEDURE F*(): INTEGER;
            BEGIN RETURN r.a.b END F
            END Bad.";
        assert!(lower_module(invalid).is_err());
        let unsupported="MODULE Bad;
            TYPE R = RECORD a: ARRAY 3 OF INTEGER END;
            VAR r: R; END Bad.";
        assert!(lower_module(unsupported).is_err());
    }

    #[test]
    fn shadowed_inherited_record_fields_fail_closed() {
        // Source Oberon may accept a shadowed inherited field with a
        // warning, but the native flattened offsets must never pick an
        // arbitrary old or new field declaration.
        let hidden="MODULE Hidden;
            TYPE Base = RECORD x: INTEGER END;
                 Child = RECORD (Base) x: INTEGER END;
            VAR child: Child;
            PROCEDURE F*(): INTEGER;
            BEGIN child.x:=7; RETURN child.x END F;
            END Hidden.";
        assert!(lower_module(hidden).is_err());
        let nested="MODULE HiddenNested;
            TYPE Inner = RECORD x: INTEGER END;
                 Base = RECORD member: Inner END;
                 Child = RECORD (Base) member: Inner END;
            VAR child: Child;
            END HiddenNested.";
        assert!(lower_module(nested).is_err());
    }

    #[test]
    fn record_projection_rejects_unrepresentable_layouts_and_operations() {
        let exported = "MODULE M; TYPE Pair = RECORD x: INTEGER END;
            VAR p*: Pair; END M.";
        assert!(lower_module(exported).is_err());
        let pointer = "MODULE M; TYPE R = RECORD p: POINTER TO R END;
            VAR r: R; END M.";
        assert!(lower_module(pointer).is_err());
        let bad_extension = "MODULE M;
            TYPE Base = RECORD x: INTEGER END;
                 Derived = RECORD (Base) y: REAL END;
            VAR r: Derived; END M.";
        assert!(lower_module(bad_extension).is_err());
        let unsupported_array_copy = "MODULE M;
            VAR a,b: ARRAY 2 OF INTEGER;
            BEGIN a := b END M.";
        assert!(lower_module(unsupported_array_copy).is_err());
    }

    #[test]
    fn projects_checked_module_begin_using_globals_arrays_and_calls() {
        let source="MODULE Init;
            VAR counter*: INTEGER; data*: ARRAY 4 OF INTEGER;
            PROCEDURE Twice*(n: INTEGER): INTEGER;
            BEGIN RETURN n * 2 END Twice;
            BEGIN
                counter := Twice(11);
                data[2] := counter + 3;
                IF data[2] > 0 THEN counter := data[2] END
            END Init.";
        let module=lower_module(source).unwrap();
        let init=module.initializer.as_ref().unwrap();
        assert_eq!(init.name,"__module_init");
        assert!(init.parameters.is_empty()&&init.locals.is_empty()&&init.arrays.is_empty());
        assert_eq!(init.globals,["counter"]);
        assert_eq!(init.global_arrays[0].length,4);
        assert!(matches!(&init.body[0],Stmt::Assign{variable:0,
            value:Expr::Call{name,args}} if name=="Twice"&&args.len()==1));
        assert!(matches!(&init.body[1],Stmt::AssignArray{array:0,..}));
        assert!(matches!(&init.body[2],Stmt::If{..}));
        assert!(module.functions.iter().all(|f|f.name!="__module_init"));
    }

    #[test]
    fn bodyless_and_body_only_modules_remain_distinct() {
        let old=lower_module("MODULE M; VAR n*: INTEGER; END M.").unwrap();
        assert!(old.initializer.is_none());
        let source="MODULE M;
            VAR n*: INTEGER;
            BEGIN n := 47
            END M.";
        let init=lower_module(source).unwrap();
        assert!(matches!(init.initializer.unwrap().body.as_slice(),
            [Stmt::Assign{variable:0,value:Expr::Integer(47)}]));
        let invalid="MODULE M; VAR n*: INTEGER;
            BEGIN n := TRUE END M.";
        assert!(lower_module(invalid).is_err());
    }

    #[test]
    fn lowers_checked_integer_calls_and_rejects_bad_signatures() {
        let src="MODULE Calls;
            PROCEDURE Add*(a,b: INTEGER): INTEGER;
            BEGIN RETURN a+b END Add;
            PROCEDURE Twice*(n: INTEGER): INTEGER;
            BEGIN RETURN Add(n,n) END Twice;
        END Calls.";
        let module=lower_module(src).unwrap();
        assert_eq!(module.functions.len(),2);
        assert!(matches!(&module.functions[1].body[0],
            Stmt::Return(Expr::Call{name,args}) if
                name=="Add" && args.len()==2));
        // The semantic checker follows Oberon's declaration-order rule:
        // calling a later procedure without a prior declaration is invalid.
        let forward="MODULE Calls;
            PROCEDURE Twice*(n: INTEGER): INTEGER;
            BEGIN RETURN Add(n,n) END Twice;
            PROCEDURE Add*(a,b: INTEGER): INTEGER;
            BEGIN RETURN a+b END Add;
        END Calls.";
        assert!(lower_module(forward).is_err());
        let bad="MODULE Calls;
            PROCEDURE Add*(a,b: INTEGER): INTEGER;
            BEGIN RETURN a+b END Add;
            PROCEDURE Wrong*(a: INTEGER): INTEGER;
            BEGIN RETURN Add(a) END Wrong;
        END Calls.";
        assert!(lower_module(bad).is_err());
        let qualified="MODULE Calls; IMPORT Out;
            PROCEDURE Wrong*(a: INTEGER): INTEGER;
            BEGIN Out.Int(a); RETURN a END Wrong;
        END Calls.";
        assert!(lower_module(qualified).is_err());
    }

    #[test]
    fn lowers_checked_arithmetic_procedures() {
        let m = lower_module("MODULE Math; PROCEDURE Sum*(x, y: INTEGER): INTEGER; \
            BEGIN RETURN x + y * 3 END Sum; END Math.").unwrap();
        assert_eq!(m.name, "Math");
        assert_eq!(m.functions[0].parameters, ["x", "y"]);
        assert!(matches!(m.functions[0].body[0], Stmt::Return(Expr::Binary(Op::Add, _, _))));
    }

    #[test]
    fn lowers_integer_locals_if_and_while() {
        let m = lower_module("MODULE M; PROCEDURE Count*(n: INTEGER): INTEGER; \
            VAR result: INTEGER; BEGIN result := 0; \
            WHILE n > 0 DO IF n > 2 THEN result := result + 2 \
            ELSE result := result + 1 END; n := n - 1 END; \
            RETURN result END Count; END M.").unwrap();
        let p = &m.functions[0];
        assert_eq!(p.parameters, ["n"]);
        assert_eq!(p.locals, ["result"]);
        assert!(matches!(p.body[1], Stmt::While { .. }));
    }

    #[test]
    fn lowers_checked_fixed_arrays_and_indexed_expressions() {
        let src="MODULE Arrays; PROCEDURE Sum*(n: INTEGER): INTEGER;
            VAR arr: ARRAY 4 OF INTEGER; other: INTEGER;
            BEGIN arr[0] := n; arr[1] := arr[0] + 2;
            other := arr[1]; RETURN other END Sum; END Arrays.";
        let m=lower_module(src).unwrap();
        let p=&m.functions[0];
        assert_eq!(p.arrays,vec![ArrayLocal{name:"arr".into(),length:4}]);
        assert_eq!(p.locals,vec!["other"]);
        assert!(matches!(&p.body[0],Stmt::AssignArray{array:0,..}));
        assert!(matches!(&p.body[1],Stmt::AssignArray{
            value:Expr::Binary(Op::Add,_,_),..}));
        let e=lower_module("MODULE A; PROCEDURE T*(x: INTEGER): INTEGER;
            VAR a: ARRAY 0 OF INTEGER;
            BEGIN RETURN x END T; END A.");
        assert!(e.is_err(),"zero-length arrays must not be silently accepted");
        let e=lower_module("MODULE A; PROCEDURE T*(x: INTEGER): INTEGER;
            VAR a: ARRAY 2, 3 OF INTEGER;
            BEGIN RETURN x END T; END A.");
        assert!(e.is_err(),"multidimensional arrays need separate layout rules");
    }

    #[test]
    fn type_errors_are_not_ignored() {
        let e = lower_module("MODULE M; PROCEDURE P*(x: INTEGER): INTEGER; \
            BEGIN RETURN unknown + x END P; END M.");
        assert!(e.is_err());
    }

    #[test]
    fn module_integer_globals_and_local_shadowing() {
        let source="MODULE Globals;
            VAR counter*, private: INTEGER;
            PROCEDURE Add*(step: INTEGER): INTEGER;
            BEGIN counter := counter + step; RETURN counter END Add;
            PROCEDURE Shadow*(step: INTEGER): INTEGER;
            VAR counter: INTEGER;
            BEGIN counter := step + 7; RETURN counter END Shadow;
            PROCEDURE Private*(step: INTEGER): INTEGER;
            BEGIN private := private + step; RETURN private END Private;
            END Globals.";
        let module=lower_module(source).unwrap();
        assert_eq!(module.globals, vec![
            Global{name:"counter".into(),exported:true},
            Global{name:"private".into(),exported:false},
        ]);
        assert_eq!(module.functions.len(), 3);
        assert_eq!(module.functions[0].globals, ["counter", "private"]);
        let add=&module.functions[0];
        // One value parameter, no locals: global counter is index 1.
        assert!(matches!(&add.body[0], Stmt::Assign{variable:1,..}));
        let shadow=&module.functions[1];
        // One param, one local: local counter is index 1, module
        // counter is index 2 and must NOT be used by this procedure.
        assert!(matches!(&shadow.body[0],Stmt::Assign{variable:1,..}));
        let private=&module.functions[2];
        assert!(matches!(&private.body[0],Stmt::Assign{variable:2,..}));
    }

    #[test]
    fn module_with_only_global_storage_is_a_valid_data_translation_unit() {
        let m=lower_module("MODULE Data; VAR counter*, hidden: INTEGER; END Data.").unwrap();
        assert!(m.functions.is_empty());
        assert_eq!(m.globals,vec![
            Global{name:"counter".into(),exported:true},
            Global{name:"hidden".into(),exported:false},
        ]);
    }

    #[test]
    fn checked_module_arrays_support_reads_stores_and_local_shadowing() {
        let src="MODULE Arrays;
            VAR shared*: ARRAY 4 OF INTEGER; hidden: ARRAY 3 OF INTEGER;
            PROCEDURE Update*(n: INTEGER): INTEGER;
            BEGIN shared[1] := n; hidden[2] := shared[1] + 2;
            RETURN hidden[2] END Update;
            PROCEDURE Shadow*(n: INTEGER): INTEGER;
            VAR shared: ARRAY 2 OF INTEGER;
            BEGIN shared[0] := n; RETURN shared[0] END Shadow;
            END Arrays.";
        let module=lower_module(src).unwrap();
        assert_eq!(module.global_arrays, vec![
            ArrayGlobal{name:"shared".into(),length:4,exported:true},
            ArrayGlobal{name:"hidden".into(),length:3,exported:false},
        ]);
        assert!(module.globals.is_empty());
        let update=&module.functions[0];
        assert!(update.arrays.is_empty());
        assert!(matches!(&update.body[0],Stmt::AssignArray{array:0,..}));
        assert!(matches!(&update.body[1],Stmt::AssignArray{array:1,..}));
        let shadow=&module.functions[1];
        assert_eq!(shadow.arrays.len(),1);
        assert!(matches!(&shadow.body[0],Stmt::AssignArray{array:0,..}));
        assert_eq!(shadow.global_arrays.len(),2);
    }

    #[test]
    fn data_only_array_module_is_accepted_but_unsupported_extents_are_rejected() {
        let module=lower_module("MODULE Data; VAR values*: ARRAY 8 OF INTEGER; END Data.").unwrap();
        assert!(module.functions.is_empty());
        assert_eq!(module.global_arrays[0].length,8);
        for dim in ["0", "1025", "2, 2"] {
            let src=format!("MODULE Data; VAR values: ARRAY {dim} OF INTEGER; END Data.");
            assert!(lower_module(&src).is_err());
        }
    }

    #[test]
    fn unsupported_global_types_are_rejected() {
        let array="MODULE M; VAR a: ARRAY 3, 2 OF INTEGER;
            PROCEDURE R*(v: INTEGER): INTEGER;
            BEGIN RETURN v END R; END M.";
        assert!(lower_module(array).unwrap_err().contains("fixed INTEGER arrays"));
        let real="MODULE M; VAR a: REAL;
            PROCEDURE R*(v: INTEGER): INTEGER;
            BEGIN RETURN v END R; END M.";
        assert!(lower_module(real).is_err());
    }

    #[test]
    fn unsupported_operations_fail_closed() {
        let e = lower_module("MODULE M; PROCEDURE P*(x: INTEGER): INTEGER; \
            BEGIN RETURN x DIV 3 END P; END M.");
        assert!(e.unwrap_err().contains("unsupported binary"));
        let e = lower_module("MODULE M; PROCEDURE P*(x: INTEGER): INTEGER; \
            VAR z: REAL; BEGIN RETURN x END P; END M.");
        assert!(e.unwrap_err().contains("INTEGER local"));
    }
}

#[cfg(test)]
mod checked_procedure_exports_tests {
    use super::*;

    #[test]
    fn source_checked_exports_preserve_public_private_and_old_api() {
        let source = "MODULE M;
            PROCEDURE Private(x: INTEGER): INTEGER;
            BEGIN RETURN x + 1 END Private;
            PROCEDURE Public*(x: INTEGER): INTEGER;
            BEGIN RETURN Private(x) END Public;
            END M.";
        let (module, exports) = lower_module_with_exports(source).unwrap();
        assert_eq!(exports, vec!["Public".to_string()]);
        assert_eq!(module.functions.len(), 2);
        assert_eq!(lower_module(source).unwrap(), module);
    }
}
