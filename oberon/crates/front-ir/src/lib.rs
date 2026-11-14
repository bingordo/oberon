//! Backend-independent projection of checked Oberon procedures.
//!
//! Only syntax and semantic-analysis crates are dependencies. Unsupported
//! constructs fail closed; the consuming compiler owns SSA and code generation.
use std::collections::HashMap;

pub use sema::sym::{Interface,Item as InterfaceItem};
pub fn parse_interface(text:&str)->Result<Interface,String>{
    sema::sym::parse(text)
}
pub fn render_interface(interface:&Interface)->String {
    sema::sym::render(interface)
}

/// Parse the actual Oberon module dependency list before interfaces are
/// available. Semantic admission always happens later through sema with all
/// resolved, checked interfaces; these names never grant codegen authority.
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct ModuleDependencies {
    pub module:String,
    pub imports:Vec<String>,
    /// A source BEGIN body is not safe for independent multi-module object
    /// linking without a defined initializer dependency order.
    pub has_initializer:bool,
}
pub fn module_dependencies(source:&str)->Result<ModuleDependencies,String> {
    let parsed=syntax::parse_module(source)
        .map_err(|error|format!("Oberon dependency parse error: {error:?}"))?;
    Ok(ModuleDependencies {
        module:parsed.name,
        imports:parsed.imports.iter().map(|import|import.module.clone()).collect(),
        has_initializer:!parsed.body.is_empty(),
    })
}

/// Only the ABI we lower natively is advertised in this initial .sym writer:
/// exported INTEGER-returning functions taking value/VAR INTEGER scalars.
/// Other exports must not silently disappear from an interface file.
pub fn checked_scalar_interface(source:&str)->Result<Interface,String> {
    checked_scalar_interface_with_imports(source,&[])
}
pub fn checked_scalar_interface_with_imports(
    source:&str,interfaces:&[Interface],
)->Result<Interface,String> {
    let (checked,diagnostics)=sema::check_module_recovering(source,interfaces);
    if let Some(diagnostic)=diagnostics.iter().find(|d|d.is_error()) {
        return Err(format!("Oberon interface semantic or syntax error: {diagnostic:?}"));
    }
    let module=checked.ok_or("missing checked Oberon module")?;
    if module.decls.types.iter().any(|x|x.export) ||
       module.decls.vars.iter().any(|x|x.names.iter().any(|(_,export)|*export)) ||
       module.decls.consts.iter().any(|x|x.export) {
        return Err("native scalar .sym writer cannot hide non-procedure exports".into());
    }
    let mut items=Vec::new();
    for proc in &module.decls.procs {
        if !proc.export {continue;}
        if !matches!(proc.result.as_deref(),Some([ty]) if ty=="INTEGER") {
            return Err("native scalar .sym requires INTEGER result".into());
        }
        let mut params=Vec::new();
        for section in &proc.params {
            if section.ty.open!=0 || section.ty.base.as_slice()!=["INTEGER"] {
                return Err("native scalar .sym supports INTEGER and VAR INTEGER formals only".into());
            }
            for _ in &section.names {
                params.push(if section.by_ref {"VAR INTEGER"} else {"INTEGER"}.into());
            }
        }
        items.push(InterfaceItem::Proc {
            name:proc.name.clone(),params,result:Some("INTEGER".into()),
        });
    }
    Ok(Interface {
        module:module.name,
        imports:module.imports.iter().map(|import|import.module.clone()).collect(),
        items,
    })
}

use syntax::ast::{BinOp, Expr as OberonExpr, ExprKind, Stmt as OberonStmt,
                  StmtKind, TypeExpr, UnOp};
use sema::const_eval::{self, Const as CheckedConst};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Module {
    pub name: String,
    /// Module-level INTEGER objects, in source order.
    pub globals: Vec<Global>,
    pub global_arrays: Vec<ArrayGlobal>,
    /// Private checked pointer-to-record storage; never an exported ABI type.
    pub pointer_globals: Vec<PointerObject>,
    pub functions: Vec<Function>,
    /// Source-checked exported imported scalar function declarations.
    pub imported_procedures: Vec<ImportedProcedure>,
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
pub struct ImportedProcedure {
    pub module: String,
    pub name: String,
    pub parameter_by_ref: Vec<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Function {
    pub name: String,
    /// Value INTEGER parameters, in source order.
    pub parameters: Vec<String>,
    /// True for VAR INTEGER, false for value INTEGER. One entry per name.
    pub parameter_by_ref: Vec<bool>,
    /// Zero-initialized INTEGER locals, in source order.
    pub locals: Vec<String>,
    /// Fixed-size single-dimensional local INTEGER arrays. Nonzero lengths.
    pub arrays: Vec<ArrayLocal>,
    /// Module arrays follow locals in array-index numbering.
    pub global_arrays: Vec<ArrayGlobal>,
    pub pointer_locals: Vec<PointerObject>,
    pub pointer_globals: Vec<PointerObject>,
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

/// Nominally checked pointer-to-private-record declaration. Field offsets,
/// types and byte length come exclusively from source-checked record layouts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PointerField {
    pub path: String,
    pub offset_bytes: u32,
    pub bits: u8,
    /// Required for chained typed dereferences, never inferred from bits.
    pub target_record: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PointerObject {
    pub name: String,
    pub record: String,
    pub bytes: u32,
    pub fields: Vec<PointerField>,
}
type PointerBindings = HashMap<String,(usize,PointerObject)>;
type PointerTargets = HashMap<String,PointerObject>;
struct PointerContext<'a> {
    objects: &'a PointerBindings,
    targets: &'a PointerTargets,
}

#[derive(Clone,Debug)]
struct PointerAccess {
    root:usize,
    /// Offsets of pointer-valued fields traversed before the selected field.
    hops:Vec<u32>,
    offset:u32,
    bits:u8,
    target:Option<String>,
}


#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expr {
    Variable(usize),
    /// Address of a checked assignable INTEGER scalar, exclusively for VAR calls.
    AddressOfScalar(usize),
    /// Index into Function.arrays; the subscript is a checked INTEGER expr.
    ArrayElement { array: usize, index: Box<Expr> },
    /// Source-checked fixed INTEGER array, flattened into one native
    /// contiguous word allocation. Each dimension must be checked on its
    /// own: an in-range flattened offset is not proof of valid subscripts.
    MultiArrayElement { array: usize, indices: Vec<Expr>, dims: Vec<u32> },
    /// Source-checked private record leaf: exact byte offset from its
    /// existing declared local/global storage root, with width 8 or 32.
    /// No free pointer, by-value record ABI or arbitrary byte access.
    RecordField { array: usize, byte_offset: u32, bits: u8 },
    /// Checked dereference of a private pointer to a source-checked record.
    PointerField { pointer: usize, byte_offset: u32, bits: u8 },
    PointerChainField {pointer:usize,hops:Vec<u32>,byte_offset:u32,bits:u8},
    /// Full-width pointer from a local/global object or a checked link.
    PointerValue {pointer:usize,hops:Vec<u32>,byte_offset:Option<u32>},
    NilPointer,
    PointerNullTest {value:Box<Expr>,equal:bool},
    Integer(i32),
    Boolean(bool),
    Binary(Op, Box<Expr>, Box<Expr>),
    /// Checked same-module INTEGER function call, including recursion.
    Call { name:String, args:Vec<Expr> },
    /// Direct call to an imported source-checked scalar function; the defining
    /// module name is authoritative, and the alias is not part of its symbol.
    ImportedCall {module:String,name:String,args:Vec<Expr>},
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
    AssignMultiArray { array: usize, indices: Vec<Expr>, dims: Vec<u32>, value: Expr },
    /// Ordered byte-exact private record leaf write; widths 8/32 only.
    AssignRecordField { array: usize, byte_offset: u32, bits: u8, value: Expr },
    AssignPointerField {pointer:usize,byte_offset:u32,bits:u8,value:Expr},
    NewPointer {pointer:usize,bytes:u32},
    AssignPointer {pointer:usize,source:Option<usize>},
    AssignPointerValue {pointer:usize,value:Expr},
    AssignPointerChainField {
        pointer:usize,hops:Vec<u32>,byte_offset:u32,bits:u8,value:Expr,
    },
    NewPointerLink {pointer:usize,hops:Vec<u32>,byte_offset:u32,bytes:u32},
    /// Checked record/subrecord byte range, including padding. The frontend
    /// proves both nominal projections and range bounds; reverse memmove
    /// ordering is required only when two ranges overlap within one object.
    CopyRecordBytes {
        dst_array: usize, dst_byte_offset: u32,
        src_array: usize, src_byte_offset: u32,
        byte_length: u32, reverse: bool,
    },
    Return(Expr),
    /// Evaluate a procedure call for its effect, ignoring its result.
    Call(Expr),
    If { condition: Expr, then_body: Vec<Stmt>, else_body: Vec<Stmt> },
    While { condition: Expr, body: Vec<Stmt> },
}

// Private homogeneous INTEGER records now use contiguous checked memory,
// with 4-byte fields and an addressable allocation root. No record ABI,
// pointer exposure, record extension or heterogeneous fields are inferred.
fn record_fields(ty: &TypeExpr, types: &HashMap<String, Vec<String>>,
    pointers:&HashMap<String,String>)
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
                let scalar=matches!(&field.ty,
                    TypeExpr::Named(q,_) if q.as_slice()==["INTEGER"] ||
                        q.as_slice()==["BYTE"] ||
                        (q.len()==1 && pointers.contains_key(&q[0])))
                    || matches!(&field.ty,TypeExpr::Pointer(_,_));
                let nested=if scalar {None} else {
                    Some(record_fields(&field.ty,types,pointers)?
                        .ok_or("record fields must use checked scalar, pointer or record types")?)
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
    /// A full-width pointer to a nominal record; the pointee is not embedded
    /// and may be declared later (including self-referential record cycles).
    Pointer(String),
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
    if !matches!(alignment,1|4|8) {
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
    pointers: &HashMap<String,String>,
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
                    let child = checked_record_layout(&field.ty,&child_id,named,pointers,layouts)?;
                    let kind = match (&field.ty, child) {
                        (TypeExpr::Named(q, _), None) if q.as_slice() == ["INTEGER"] =>
                            CheckedRecordFieldType::Integer,
                        (TypeExpr::Named(q, _), None) if q.as_slice() == ["BYTE"] =>
                            CheckedRecordFieldType::Byte,
                        (_, Some(id)) => CheckedRecordFieldType::Record(id),
                        (TypeExpr::Named(q,_),None) if q.len()==1 &&
                            pointers.contains_key(&q[0]) =>
                            CheckedRecordFieldType::Pointer(pointers[&q[0]].clone()),
                        (TypeExpr::Pointer(_,_),None) =>
                            CheckedRecordFieldType::Pointer(
                                pointer_record_target(&field.ty,pointers,named)?
                                    .ok_or("pointer field has no nominal record target")?),
                        _ => return Err("checked record field needs INTEGER, BYTE, pointer or record".into()),
                    };
                    let (bytes,align) = match &kind {
                        CheckedRecordFieldType::Integer => (4,4),
                        CheckedRecordFieldType::Byte => (1,1),
                        CheckedRecordFieldType::Pointer(_) => (8,8),
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
struct RecordRegion {
    path: String,
    offset: u32,
    size: u32,
}
#[derive(Clone,Debug)]
struct RecordShape {
    fields: Vec<String>,
    bytes: Vec<(u32,u8)>,
    /// Every named nested record and the root; record leaves are excluded.
    regions: Vec<RecordRegion>,
    has_byte: bool,
    words: u32,
}

fn record_shape(
    identity:&str, layouts:&[CheckedRecordLayout], legacy_fields:&[String],
) -> Result<RecordShape,String> {
    fn visit(
        id:&str,prefix:&str,offset:u32,layouts:&[CheckedRecordLayout],
        out:&mut Vec<(String,u32,u8)>,
        regions:&mut Vec<RecordRegion>,
    )->Result<(),String> {
        let record=layouts.iter().find(|r|r.declaration==id)
            .ok_or("checked record object refers to missing record declaration")?;
        if let Some(base)=&record.base {
            visit(base,prefix,offset,layouts,out,regions)?;
        }
        for field in &record.fields {
            let field_offset=offset.checked_add(field.offset_bytes)
                .ok_or("record field byte offset overflow")?;
            let path=format!("{prefix}{}",field.name);
            match &field.ty {
                CheckedRecordFieldType::Integer=>out.push((path,field_offset,32)),
                CheckedRecordFieldType::Byte=>out.push((path,field_offset,8)),
                CheckedRecordFieldType::Pointer(_)=>out.push((path,field_offset,64)),
                CheckedRecordFieldType::Record(child)=>{
                    let nested=layouts.iter().find(|r|&r.declaration==child)
                        .ok_or("checked nested region layout missing")?;
                    regions.push(RecordRegion {
                        path:path.clone(),offset:field_offset,size:nested.size_bytes,
                    });
                    visit(child,&format!("{path}."),field_offset,layouts,out,regions)?;
                },
            }
        }
        Ok(())
    }
    let layout=layouts.iter().find(|r|r.declaration==identity)
        .ok_or("record object layout missing")?;
    // The frontend's array index is a *storage handle*, not a license
    // to round object bounds up to machine words. Native lowering reserves
    // ceil(bytes/4) frame words, but proves the original byte extent using
    // LocalByteArrayAddr for partial-word objects.
    if layout.size_bytes==0 || layout.alignment_bytes>4 {
        return Err("unsupported private record byte extent or alignment".into());
    }
    let mut leaves=Vec::new();
    let mut regions=vec![RecordRegion {
        path:String::new(),offset:0,size:layout.size_bytes,
    }];
    visit(identity,"",0,layouts,&mut leaves,&mut regions)?;
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
        regions,has_byte,words:size.div_ceil(4),
    })
}

/// This vertical slice admits only pointer variables whose target is a
/// previously source-checked named private record. No pointer parameters,
/// imported targets, recursive inline declarations or pointer arithmetic.
fn pointer_record_target(
    ty:&TypeExpr, pointers:&HashMap<String,String>,
    records:&HashMap<String,String>,
)->Result<Option<String>,String> {
    match ty {
        TypeExpr::Named(q,_) if q.len()==1 => Ok(pointers.get(&q[0]).cloned()),
        TypeExpr::Pointer(inner,_) => match inner.as_ref() {
            TypeExpr::Named(q,_) if q.len()==1 => records.get(&q[0])
                .cloned().map(Some)
                .ok_or_else(||"native pointer target must be a checked named record".into()),
            _=>Err("native pointer target must be a named private record".into()),
        },
        _=>Ok(None),
    }
}
fn checked_pointer_object(
    name:&str,record:&str, layouts:&[CheckedRecordLayout],
    types:&HashMap<String,Vec<String>>,
)->Result<PointerObject,String> {
    fn visit(
        id:&str,prefix:&str,base_offset:u32,layouts:&[CheckedRecordLayout],
        out:&mut Vec<PointerField>,
    )->Result<(),String> {
        let layout=layouts.iter().find(|r|r.declaration==id)
            .ok_or("pointer target nested record layout missing")?;
        if let Some(base)=&layout.base {
            visit(base,prefix,base_offset,layouts,out)?;
        }
        for field in &layout.fields {
            let offset=base_offset.checked_add(field.offset_bytes)
                .ok_or("pointer field offset overflow")?;
            let path=format!("{prefix}{}",field.name);
            match &field.ty {
                CheckedRecordFieldType::Integer=>out.push(PointerField {
                    path,offset_bytes:offset,bits:32,target_record:None,
                }),
                CheckedRecordFieldType::Byte=>out.push(PointerField {
                    path,offset_bytes:offset,bits:8,target_record:None,
                }),
                CheckedRecordFieldType::Pointer(target)=>out.push(PointerField {
                    path,offset_bytes:offset,bits:64,target_record:Some(target.clone()),
                }),
                CheckedRecordFieldType::Record(child)=>visit(
                    child,&format!("{path}."),offset,layouts,out)?,
            }
        }
        Ok(())
    }
    let record_name=record.rsplit('.').next()
        .ok_or("pointer record identity is empty")?;
    let expected=types.get(record_name)
        .ok_or("pointer target has no checked record fields")?;
    let layout=layouts.iter().find(|r|r.declaration==record)
        .ok_or("pointer target layout missing")?;
    if layout.size_bytes==0 || layout.size_bytes>4096 ||
        !matches!(layout.alignment_bytes,1|4|8) {
        return Err("unsupported pointer target extent/alignment".into());
    }
    let mut fields=Vec::new();
    visit(record,"",0,layouts,&mut fields)?;
    if fields.iter().map(|f|&f.path).ne(expected.iter()) {
        return Err("pointer target checked leaf paths disagree with record layout".into());
    }
    for field in &fields {
        if field.offset_bytes.checked_add(u32::from(field.bits/8))
            .is_none_or(|end|end>layout.size_bytes) {
            return Err("pointer field crosses checked record byte extent".into());
        }
    }
    Ok(PointerObject {
        name:name.into(),record:record.into(),bytes:layout.size_bytes,fields,
    })
}
fn resolve_pointer_access(
    d:&syntax::ast::Designator,pointers:&PointerBindings,
    targets:&PointerTargets,
)->Result<PointerAccess,String> {
    if d.base.len()!=1 || d.sels.len()<2 {
        return Err("checked record-pointer field requires ^ and a field".into());
    }
    let (root,object)=pointers.get(&d.base[0])
        .ok_or("pointer field starts from an unknown checked object")?;
    let mut record=object.record.clone();
    let mut hops=Vec::new();
    let mut cursor=0;
    loop {
        if !matches!(d.sels.get(cursor),Some(syntax::ast::Selector::Deref)) {
            return Err("pointer record chain must explicitly dereference ^".into());
        }
        cursor+=1;
        let mut names=Vec::new();
        while let Some(syntax::ast::Selector::Field(name))=d.sels.get(cursor) {
            names.push(name.as_str());
            cursor+=1;
        }
        if names.is_empty(){return Err("pointer dereference needs a field".into());}
        let shape=targets.get(&record)
            .ok_or("checked pointer target record layout unavailable")?;
        let path=names.join(".");
        let field=shape.fields.iter().find(|f|f.path==path)
            .ok_or("pointer field is absent from checked record layout")?;
        if cursor==d.sels.len() {
            return Ok(PointerAccess {
                root:*root,hops,offset:field.offset_bytes,bits:field.bits,
                target:field.target_record.clone(),
            });
        }
        if cursor>d.sels.len() || hops.len()>=16 {
            return Err("pointer record selector chain is unsupported or too deep".into());
        }
        let target=field.target_record.as_ref()
            .ok_or("pointer chain can only traverse pointer-valued fields")?;
        hops.push(field.offset_bytes);
        record=target.clone();
    }
}
fn pointer_value(
    expr:&OberonExpr,pointers:&PointerBindings,
    targets:&PointerTargets,
)->Result<Option<(Expr,String)>,String> {
    match &expr.kind {
        ExprKind::Nil=>Ok(None),
        ExprKind::Designator(d) if d.base.len()==1 && d.sels.is_empty()=>{
            let (index,object)=pointers.get(&d.base[0])
                .ok_or("expected source-checked pointer variable")?;
            Ok(Some((Expr::PointerValue {
                pointer:*index,hops:Vec::new(),byte_offset:None,
            },object.record.clone())))
        },
        ExprKind::Designator(d)=>{
            let access=resolve_pointer_access(d,pointers,targets)?;
            let target=access.target.ok_or("expected pointer-valued record field")?;
            Ok(Some((Expr::PointerValue {
                pointer:access.root,hops:access.hops,
                byte_offset:Some(access.offset),
            },target)))
        },
        _=>Err("pointer value must be NIL or a checked pointer designator".into()),
    }
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

/// Resolve only checked *record-valued* regions, never scalar field paths.
/// For derived-to-base assignment, the source region may be larger but its
/// field prefix must preserve names, widths and *relative byte offsets*.
struct RecordByteRegion {
    array: usize,
    offset: u32,
    size: u32,
    leaves: Vec<(String,u32,u8)>,
}
fn record_byte_region(
    records:&RecordObjects,root:&str,path:&str,
)->Option<RecordByteRegion> {
    let (array,shape)=records.get(root)?;
    let region=shape.regions.iter().find(|r|r.path==path)?;
    let prefix=if path.is_empty() {String::new()} else {format!("{path}.")};
    let mut leaves=Vec::new();
    for (name,(offset,width)) in shape.fields.iter().zip(&shape.bytes) {
        if let Some(relative_name)=name.strip_prefix(&prefix) {
            let relative_offset=offset.checked_sub(region.offset)?;
            if relative_offset.checked_add(u32::from(*width)/8)?>region.size {
                return None;
            }
            leaves.push((relative_name.to_string(),relative_offset,*width));
        }
    }
    if leaves.is_empty() || region.offset.checked_add(region.size)?>shape.words*4 {
        return None;
    }
    Some(RecordByteRegion {
        array:*array,offset:region.offset,size:region.size,leaves,
    })
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
    // Byte-based copies retain *all* bytes of the selected destination
    // region, including padding. The existing leaf-word scheme would be
    // unsound when a BYTE field changes the memory stride.
    if records.get(&target.base[0]).is_some_and(|(_,s)|s.has_byte) ||
       records.get(&src.base[0]).is_some_and(|(_,s)|s.has_byte) {
        let dst=record_byte_region(records,&target.base[0],&dst_path)
            .ok_or("selected destination is not a checked record byte region")?;
        let src=record_byte_region(records,&src.base[0],&src_path)
            .ok_or("selected source is not a checked record byte region")?;
        let (da,doff,dlen,df)=(dst.array,dst.offset,dst.size,&dst.leaves);
        let (sa,soff,slen,sf)=(src.array,src.offset,src.size,&src.leaves);
        if slen<dlen || sf.len()<df.len() ||
           !sf.iter().zip(df.iter()).all(|(a,b)|a==b) {
            return Err("record byte copy requires a compatible source record prefix".into());
        }
        // Sema has already proven source-to-destination type assignability.
        // The extra verified layout match above prevents partial leaf copies,
        // stale field offsets and reinterpreting non-record designators.
        if da==sa && doff==soff {
            return Ok(Some(Vec::new()));
        }
        let src_end=soff.checked_add(dlen).ok_or("copy region byte overflow")?;
        let reverse=da==sa && doff>soff && doff<src_end;
        return Ok(Some(vec![Stmt::CopyRecordBytes {
            dst_array:da,dst_byte_offset:doff,
            src_array:sa,src_byte_offset:soff,
            byte_length:dlen,reverse,
        }]));
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

/// Use the existing Oberon semantic constant folder for both declarations
/// and array bounds. Native lowering only admits values it can represent;
/// it never reimplements DIV/MOD, overflow or predeclared-function rules.
type CheckedConstants = HashMap<String,CheckedConst>;
type ArrayBindings<'a> = HashMap<&'a str,(usize,Vec<u32>)>;

fn lookup_constant(expr:&OberonExpr, constants:&CheckedConstants)->Option<CheckedConst> {
    let ExprKind::Designator(d)=&expr.kind else {return None};
    if d.base.len()!=1 || !d.sels.is_empty() {return None}
    constants.get(&d.base[0]).cloned()
}

fn checked_constants(
    declarations:&[syntax::ast::ConstDecl],
    outer:&CheckedConstants,
)->Result<CheckedConstants,String> {
    let mut constants=outer.clone();
    for declaration in declarations {
        let value=const_eval::eval_with(&declaration.value,&|expr|
            lookup_constant(expr,&constants))
            .map_err(|err|format!("constant {}: {err}",declaration.name))?;
        if !matches!(value,CheckedConst::Int(_) | CheckedConst::Bool(_)) {
            return Err(format!(
                "constant {}: native bridge supports only INTEGER and BOOLEAN values",
                declaration.name));
        }
        constants.insert(declaration.name.clone(),value);
    }
    Ok(constants)
}

/// A single verified source dimension maps to one explicit runtime bounds
/// check. The total is limited by the existing local frame/native array cap.
fn checked_array_dimensions(
    dims:&[OberonExpr],constants:&CheckedConstants,
)->Result<(Vec<u32>,u32),String> {
    if dims.is_empty() || dims.len()>3 {
        return Err("native fixed INTEGER arrays require 1..=3 dimensions".into());
    }
    let mut total=1u32;
    let mut lengths=Vec::with_capacity(dims.len());
    for dim in dims {
        let length=checked_array_length(dim,constants)?;
        total=total.checked_mul(length)
            .ok_or("native fixed INTEGER array extent overflow")?;
        if total>1024 {
            return Err("native fixed INTEGER array exceeds 1024 elements".into());
        }
        lengths.push(length);
    }
    Ok((lengths,total))
}

fn checked_array_length(
    dim:&OberonExpr,constants:&CheckedConstants,
)->Result<u32,String> {
    match const_eval::eval_with(dim,&|expr|lookup_constant(expr,constants)) {
        Ok(CheckedConst::Int(n)) if (1..=1024).contains(&n) => Ok(n as u32),
        Ok(_) => Err("native INTEGER array extent must be a constant in 1..=1024".into()),
        Err(err) => Err(format!("native INTEGER array extent: {err}")),
    }
}

#[derive(Clone,Debug)]
struct CallSignature {
    parameter_by_ref:Vec<bool>,
    module:Option<String>,
}

fn lower_call(d:&syntax::ast::Designator,
              names:&HashMap<&str,usize>,
              arrays:&ArrayBindings<'_>,
              records:&RecordObjects,
              pointers:&PointerContext<'_>,
              functions:&HashMap<String,CallSignature>,
              constants:&CheckedConstants)->Result<Expr,String> {
    if !(1..=2).contains(&d.base.len()) || d.sels.len()!=1 {
        return Err("unsupported qualified or selected function call".into());
    }
    let symbol=d.base.join(".");
    let expected=functions.get(&symbol)
        .ok_or_else(||format!("unsupported or non-exported function {symbol}"))?;
    let raw_args=match &d.sels[0] {
        syntax::ast::Selector::Call(args)=>args.as_slice(),
        syntax::ast::Selector::Args(args)=>args.as_deref().unwrap_or(&[]),
        _=>return Err("expected direct function call".into()),
    };
    if raw_args.len()!=expected.parameter_by_ref.len() {
        return Err(format!("{symbol}: call argument count mismatch"));
    }
    let mut args=Vec::with_capacity(raw_args.len());
    for (arg,&by_ref) in raw_args.iter().zip(&expected.parameter_by_ref) {
        if by_ref {
            let ExprKind::Designator(actual)=&arg.kind else {
                return Err("VAR INTEGER argument must be assignable scalar designator".into());
            };
            if actual.base.len()!=1 || !actual.sels.is_empty() {
                return Err("VAR INTEGER argument must be a direct scalar object".into());
            }
            let index=names.get(actual.base[0].as_str())
                .ok_or("VAR INTEGER argument is not an addressable checked scalar")?;
            args.push(Expr::AddressOfScalar(*index));
        } else {
            args.push(lower_expr(arg,names,arrays,records,pointers,functions,constants)?);
        }
    }
    if let Some(module)=&expected.module {
        Ok(Expr::ImportedCall {
            module:module.clone(),name:d.base[1].clone(),args,
        })
    } else {
        Ok(Expr::Call{name:symbol,args})
    }
}

fn lower_expr(expr: &OberonExpr, names: &HashMap<&str, usize>,
              arrays: &ArrayBindings<'_>,
              records: &RecordObjects,
              pointers:&PointerContext<'_>,
              functions:&HashMap<String,CallSignature>,
              constants:&CheckedConstants) -> Result<Expr, String> {
    match &expr.kind {
        ExprKind::Designator(d) if d.base.len()==1 &&
            d.sels.iter().any(|sel|matches!(sel,syntax::ast::Selector::Deref)) => {
            let access=resolve_pointer_access(d,pointers.objects,pointers.targets)?;
            if access.bits==64 {
                Ok(Expr::PointerValue {
                    pointer:access.root,hops:access.hops,
                    byte_offset:Some(access.offset),
                })
            } else if access.hops.is_empty() {
                Ok(Expr::PointerField {
                    pointer:access.root,byte_offset:access.offset,bits:access.bits,
                })
            } else {
                Ok(Expr::PointerChainField {
                    pointer:access.root,hops:access.hops,
                    byte_offset:access.offset,bits:access.bits,
                })
            }
        }
        ExprKind::Designator(d) if (1..=2).contains(&d.base.len()) && d.sels.len()==1
            && matches!(d.sels[0],syntax::ast::Selector::Call(_)|
                        syntax::ast::Selector::Args(_)) => {
            lower_call(d,names,arrays,records,pointers,functions,constants)
        }
        ExprKind::Int(value) => i32::try_from(*value)
            .map(Expr::Integer)
            .map_err(|_| format!("INTEGER literal out of range at {}", expr.span.lo)),
        ExprKind::Bool(value) => Ok(Expr::Boolean(*value)),
        ExprKind::Nil=>Ok(Expr::NilPointer),
        ExprKind::Binary(op,left,right) if matches!(op,BinOp::Eq|BinOp::Ne) &&
            (matches!(left.kind,ExprKind::Nil) || matches!(right.kind,ExprKind::Nil)) => {
            let non_nil=if matches!(left.kind,ExprKind::Nil) {right} else {left};
            let (value,_target)=pointer_value(non_nil,pointers.objects,pointers.targets)?
                .ok_or("pointer null comparison requires a pointer operand")?;
            Ok(Expr::PointerNullTest {
                value:Box::new(value),equal:matches!(op,BinOp::Eq),
            })
        },
        ExprKind::Designator(d) if d.sels.is_empty() && d.base.len() == 1 => {
            if let Some(index)=names.get(d.base[0].as_str()) {
                Ok(Expr::Variable(*index))
            } else {
                match constants.get(&d.base[0]) {
                    Some(CheckedConst::Int(n))=>Ok(Expr::Integer(*n)),
                    Some(CheckedConst::Bool(b))=>Ok(Expr::Boolean(*b)),
                    _=>Err(format!("unsupported variable or constant: {}",d.base[0])),
                }
            }
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
                syntax::ast::Selector::Index(indices) => {
                    let (array,dims)=arrays.get(d.base[0].as_str())
                        .ok_or_else(||format!("not a supported local array: {}",d.base[0]))?;
                    if indices.len()!=dims.len() {
                        return Err("fixed INTEGER array requires exactly one index per dimension".into());
                    }
                    let mut lowered=Vec::with_capacity(indices.len());
                    for index in indices {
                        lowered.push(lower_expr(index,names,arrays,records,pointers,functions,constants)?);
                    }
                    if dims.len()==1 {
                        Ok(Expr::ArrayElement {
                            array:*array,index:Box::new(lowered.remove(0)),
                        })
                    } else {
                        Ok(Expr::MultiArrayElement {
                            array:*array,indices:lowered,dims:dims.clone(),
                        })
                    }
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
            Ok(Expr::Binary(op, Box::new(lower_expr(a,names,arrays,records,pointers,functions,constants)?),
                            Box::new(lower_expr(b,names,arrays,records,pointers,functions,constants)?)))
        }
        ExprKind::Unary(UnOp::Plus, a) => lower_expr(a,names,arrays,records,pointers,functions,constants),
        ExprKind::Unary(UnOp::Neg, a) => {
            // The signed minimum's positive magnitude is not representable
            // as an i32. Handle it before validating positive literals.
            if let ExprKind::Int(magnitude) = &a.kind {
                return i32::try_from(-i64::from(*magnitude))
                    .map(Expr::Integer)
                    .map_err(|_| format!("INTEGER literal out of range at {}", expr.span.lo));
            }
            Ok(Expr::Binary(Op::Sub, Box::new(Expr::Integer(0)),
                            Box::new(lower_expr(a,names,arrays,records,pointers,functions,constants)?)))
        },
        _ => Err(format!("unsupported expression at {}", expr.span.lo)),
    }
}

fn lower_stmts(stmts: &[OberonStmt], names: &HashMap<&str, usize>,
              arrays: &ArrayBindings<'_>,
              records: &RecordObjects,
              pointers:&PointerContext<'_>,
              functions:&HashMap<String,CallSignature>,
              constants:&CheckedConstants) -> Result<Vec<Stmt>, String>
{
    let mut out = Vec::new();
    for s in stmts {
        match &s.kind {
            StmtKind::Call(d) if d.base.as_slice()==["NEW"] && d.sels.len()==1 => {
                let args=match &d.sels[0] {
                    syntax::ast::Selector::Call(args)=>args.as_slice(),
                    syntax::ast::Selector::Args(Some(args))=>args.as_slice(),
                    _=>return Err("NEW requires a checked pointer actual".into()),
                };
                let [argument]=args else {
                    return Err("NEW requires exactly one pointer actual".into());
                };
                let ExprKind::Designator(actual)=&argument.kind else {
                    return Err("NEW needs an assignable pointer variable".into());
                };
                if actual.base.len()!=1 {
                    return Err("NEW requires a source-checked private pointer object".into());
                }
                if actual.sels.is_empty() {
                    let (pointer,object)=pointers.objects.get(&actual.base[0])
                        .ok_or("NEW needs a checked private pointer variable")?;
                    out.push(Stmt::NewPointer {pointer:*pointer,bytes:object.bytes});
                } else {
                    let access=resolve_pointer_access(actual,pointers.objects,pointers.targets)?;
                    let record=access.target
                        .ok_or("NEW target field must have pointer-to-record type")?;
                    let shape=pointers.targets.get(&record)
                        .ok_or("NEW target record layout missing")?;
                    out.push(Stmt::NewPointerLink {
                        pointer:access.root,hops:access.hops,
                        byte_offset:access.offset,bytes:shape.bytes,
                    });
                }
            }
            StmtKind::Call(d)=>out.push(Stmt::Call(
                lower_call(d,names,arrays,records,pointers,functions,constants)?)),
            StmtKind::Assign { target, value } if target.sels.is_empty() &&
                                                   target.base.len() == 1 => {
                if let Some((pointer,object))=pointers.objects.get(&target.base[0]) {
                    let value=pointer_value(value,pointers.objects,pointers.targets)?;
                    if let Some((source,record))=value {
                        if record!=object.record {
                            return Err("pointer assignment requires same nominal record target".into());
                        }
                        if let Expr::PointerValue {pointer:from,hops,byte_offset:None}=&source {
                            if hops.is_empty() {
                                out.push(Stmt::AssignPointer {
                                    pointer:*pointer,source:Some(*from),
                                });
                            } else {
                                out.push(Stmt::AssignPointerValue {
                                    pointer:*pointer,value:source,
                                });
                            }
                        } else {
                            out.push(Stmt::AssignPointerValue {
                                pointer:*pointer,value:source,
                            });
                        }
                    } else {
                        out.push(Stmt::AssignPointer {pointer:*pointer,source:None});
                    }
                } else if let Some(copies)=lower_record_copy(target,value,records)? {
                    out.extend(copies);
                } else {
                    let variable=names.get(target.base[0].as_str()).copied()
                        .ok_or_else(||format!("unknown local or parameter: {}",target.base[0]))?;
                    out.push(Stmt::Assign {variable,value:
                        lower_expr(value,names,arrays,records,pointers,functions,constants)?});
                }
            }
            StmtKind::Assign {target,value} if target.base.len()==1 &&
                target.sels.iter().any(|sel|matches!(sel,syntax::ast::Selector::Deref)) => {
                let access=resolve_pointer_access(target,pointers.objects,pointers.targets)?;
                let val=if let Some(record)=&access.target {
                    match pointer_value(value,pointers.objects,pointers.targets)? {
                        Some((source,source_record)) if source_record==*record=>source,
                        None=>Expr::NilPointer,
                        _=>return Err("pointer-link assignment requires compatible nominal record target".into()),
                    }
                } else {
                    lower_expr(value,names,arrays,records,pointers,functions,constants)?
                };
                if access.hops.is_empty() && access.bits!=64 {
                    out.push(Stmt::AssignPointerField {
                        pointer:access.root,byte_offset:access.offset,
                        bits:access.bits,value:val,
                    });
                } else {
                    out.push(Stmt::AssignPointerChainField {
                        pointer:access.root,hops:access.hops,
                        byte_offset:access.offset,bits:access.bits,value:val,
                    });
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
                        lower_expr(value,names,arrays,records,pointers,functions,constants)?)?);
                }
            }
            StmtKind::Assign { target, value } if target.base.len()==1 &&
                                                    target.sels.len()==1 => {
                match &target.sels[0] {
                    syntax::ast::Selector::Field(field) => {
                        out.push(record_store(records,&target.base[0],field,
                            lower_expr(value,names,arrays,records,pointers,functions,constants)?)?);
                    }
                    syntax::ast::Selector::Index(indices) => {
                        let (array,dims)=arrays.get(target.base[0].as_str())
                            .ok_or_else(||format!("not a supported local array: {}",target.base[0]))?;
                        if indices.len()!=dims.len() {
                            return Err("fixed INTEGER array assignment requires one index per dimension".into());
                        }
                        let mut lowered=Vec::with_capacity(indices.len());
                        for index in indices {
                            lowered.push(lower_expr(index,names,arrays,records,pointers,functions,constants)?);
                        }
                        let value=lower_expr(value,names,arrays,records,pointers,functions,constants)?;
                        if dims.len()==1 {
                            out.push(Stmt::AssignArray {
                                array:*array,index:lowered.remove(0),value,
                            });
                        } else {
                            out.push(Stmt::AssignMultiArray {
                                array:*array,indices:lowered,dims:dims.clone(),value,
                            });
                        }
                    }
                    _=>return Err(format!("unsupported array assignment at {}",s.span.lo)),
                }
            }
            StmtKind::Return(Some(e)) =>
                out.push(Stmt::Return(lower_expr(e,names,arrays,records,pointers,functions,constants)?)),
            StmtKind::If { arms, els } => {
                if arms.is_empty() { return Err("IF with no arm".into()); }
                let mut tail = match els {
                    Some(stmts) => lower_stmts(stmts,names,arrays,records,pointers,functions,constants)?,
                    None => Vec::new(),
                };
                // ELSIF is lowered into a nested IF in the previous else arm.
                for (condition, body) in arms.iter().rev() {
                    tail = vec![Stmt::If {
                        condition: lower_expr(condition,names,arrays,records,pointers,functions,constants)?,
                        then_body: lower_stmts(body,names,arrays,records,pointers,functions,constants)?,
                        else_body: tail,
                    }];
                }
                out.extend(tail);
            }
            StmtKind::While { arms } if arms.len() == 1 => {
                let (condition, body) = &arms[0];
                out.push(Stmt::While {
                    condition: lower_expr(condition,names,arrays,records,pointers,functions,constants)?,
                    body: lower_stmts(body,names,arrays,records,pointers,functions,constants)?,
                });
            }
            StmtKind::Repeat { body, until } => {
                // Execute the body unconditionally once, and then evaluate
                // UNTIL after every iteration. Reuse the existing While IR
                // without introducing a backend-specific loop instruction.
                // EXIT remains unsupported until loops expose a break edge.
                let repeated = lower_stmts(body,names,arrays,records,pointers,functions,constants)?;
                let condition = lower_expr(until,names,arrays,records,pointers,functions,constants)?;
                out.extend(repeated.clone());
                out.push(Stmt::While {
                    condition: Expr::Binary(Op::Eq,Box::new(condition),
                                            Box::new(Expr::Boolean(false))),
                    body: repeated,
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
    -> Result<CheckedRecordProjection,String> {
    lower_module_with_record_objects_and_interfaces(source,&[])
}

/// Imported interfaces are always fed to the original source semantic
/// checker, rather than guessed from names/arity by code generation.
pub fn lower_module_with_record_objects_and_interfaces(
    source:&str,interfaces:&[Interface],
)->Result<CheckedRecordProjection,String> {
    let (checked, diagnostics) = sema::check_module_recovering(source,interfaces);
    if let Some(d) = diagnostics.iter().find(|d| d.is_error()) {
        return Err(format!("Oberon semantic or syntax error: {d:?}"));
    }
    let module = checked.ok_or("Oberon parser did not produce a module")?;
    let exported_procedures = module.decls.procs.iter()
        .filter(|proc| proc.export)
        .map(|proc| proc.name.clone())
        .collect::<Vec<_>>();
    let mut imported_procedures=Vec::<ImportedProcedure>::new();
    let mut imported_names=HashMap::<String,(String,Vec<bool>)>::new();
    for import in &module.imports {
        let interface=interfaces.iter().find(|item|item.module==import.module)
            .ok_or("no checked source interface for imported module")?;
        for item in &interface.items {
            if let InterfaceItem::Proc{name,params,result}=item {
                // Only fixed scalar INTEGER arguments/results have a native
                // TypeId ABI in this first import milestone.
                if result.as_deref()!=Some("INTEGER") {continue;}
                let modes=params.iter().map(|p|match p.as_str() {
                    "INTEGER"=>Some(false),
                    "VAR INTEGER"=>Some(true),
                    _=>None,
                }).collect::<Option<Vec<_>>>();
                let Some(modes)=modes else {continue;};
                if import.module==module.name || name.contains("__") {
                    return Err("unsupported or colliding imported scalar function".into());
                }
                imported_names.insert(format!("{}.{}",import.alias,name),
                    (import.module.clone(),modes.clone()));
                imported_procedures.push(ImportedProcedure {
                    module:import.module.clone(),name:name.clone(),
                    parameter_by_ref:modes,
                });
            }
        }
    }
    let module_constants=checked_constants(
        &module.decls.consts,&CheckedConstants::new())?;
    let mut record_types = HashMap::<String, Vec<String>>::new();
    // Predeclare record identities before visiting pointer declarations:
    // TYPE Node=POINTER TO NodeDesc; NodeDesc=RECORD next:Node END
    // is semantically legal and must not create a by-value infinite record.
    let mut record_nominals = HashMap::<String, String>::new();
    for decl in &module.decls.types {
        if matches!(decl.ty,TypeExpr::Record{..}) {
            record_nominals.insert(decl.name.clone(),
                format!("{}.{}",module.name,decl.name));
        }
    }
    let mut pointer_types=HashMap::<String,String>::new();
    // Resolve pointer aliases against the complete set of named record
    // declarations, while preserving the sema checker as type authority.
    for decl in &module.decls.types {
        if let Some(target)=pointer_record_target(
            &decl.ty,&pointer_types,&record_nominals)? {
            pointer_types.insert(decl.name.clone(),target);
        }
    }
    let mut checked_layouts = Vec::<CheckedRecordLayout>::new();
    let mut checked_objects = Vec::<CheckedRecordObject>::new();
    for decl in &module.decls.types {
        if decl.export {
            return Err("exported record types require a native ABI layout".into());
        }
        if let Some(target)=pointer_record_target(
            &decl.ty,&pointer_types,&record_nominals)? {
            pointer_types.insert(decl.name.clone(),target);
            continue;
        }
        let fields = record_fields(&decl.ty,&record_types,&pointer_types)?
            .ok_or_else(|| format!("unsupported type declaration: {}", decl.name))?;
        let declaration = format!("{}.{}", module.name, decl.name);
        let nominal = checked_record_layout(
            &decl.ty,&declaration,&record_nominals,&pointer_types,&mut checked_layouts,
        )?.ok_or("unsupported source-checked record type")?;
        record_nominals.insert(decl.name.clone(), nominal);
        record_types.insert(decl.name.clone(), fields);
    }
    let mut globals = Vec::<Global>::new();
    let mut global_arrays = Vec::<ArrayGlobal>::new();
    let mut pointer_globals=Vec::<PointerObject>::new();
    let mut global_array_dims=HashMap::<String,Vec<u32>>::new();
    let mut module_record_fields=HashMap::<String,RecordShape>::new();
    for decl in &module.decls.vars {
        if let Some(record)=pointer_record_target(
            &decl.ty,&pointer_types,&record_nominals)? {
            for (name,exported) in &decl.names {
                if *exported {return Err(
                    "exported pointer objects require explicit pointer ABI admission".into());}
                pointer_globals.push(checked_pointer_object(
                    name,&record,&checked_layouts,&record_types)?);
            }
            continue;
        }
        if let Some(fields) = record_fields(&decl.ty,&record_types,&pointer_types)? {
            let first = decl.names.first().ok_or("record variable declaration has no names")?;
            let identity = format!("{}.global.{}", module.name, first.0);
            let record = checked_record_layout(
                &decl.ty,&identity,&record_nominals,&pointer_types,&mut checked_layouts,
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
            TypeExpr::Array { dims, elem, .. } if
                matches!(elem.as_ref(),TypeExpr::Named(q,_) if q.as_slice()==["INTEGER"]) => {
                let (lengths,length)=checked_array_dimensions(dims,&module_constants)?;
                for (name, exported) in &decl.names {
                    global_arrays.push(ArrayGlobal {
                        name: name.clone(), length, exported: *exported,
                    });
                    global_array_dims.insert(name.clone(),lengths.clone());
                }
            }
            _ => return Err("only INTEGER globals and fixed INTEGER arrays are supported".into()),
        }
    }

    // Finite nominal record shapes for resolving pointer chains; the target
    // map is independent of allocation roots and never expands pointer
    // fields recursively.
    let mut pointer_targets=PointerTargets::new();
    for (name,identity) in &record_nominals {
        if record_types.contains_key(name) {
            pointer_targets.insert(identity.clone(),
                checked_pointer_object(name,identity,&checked_layouts,&record_types)?);
        }
    }

    // Collect the checked signatures before lowering procedure bodies. The
    // Oberon semantic checker already enforces source declaration order;
    // this map does not authorize forward references.
    let mut signatures=HashMap::<String,CallSignature>::new();
    for proc in &module.decls.procs {
        let by_ref=proc.params.iter()
            .flat_map(|section|std::iter::repeat_n(section.by_ref,section.names.len()))
            .collect();
        if signatures.insert(proc.name.clone(),CallSignature{
            parameter_by_ref:by_ref,module:None,
        }).is_some() {
            return Err(format!("duplicate procedure {}",proc.name));
        }
    }
    for (name,(module,modes)) in imported_names {
        if signatures.insert(name,CallSignature{
            parameter_by_ref:modes,module:Some(module),
        }).is_some() {
            return Err("ambiguous imported procedure declaration".into());
        }
    }
    let mut functions = Vec::new();
    for proc in &module.decls.procs {
        if !matches!(proc.result.as_deref(), Some([t]) if t == "INTEGER") {
            return Err(format!("{}: only INTEGER-returning functions are supported", proc.name));
        }
        if !proc.decls.procs.is_empty() || !proc.decls.types.is_empty() {
            return Err(format!("{}: nested procedures or types unsupported",proc.name));
        }
        let proc_constants=checked_constants(&proc.decls.consts,&module_constants)?;
        let mut parameters = Vec::new();
        let mut parameter_by_ref=Vec::new();
        for section in &proc.params {
            if section.ty.open != 0 || section.ty.base.as_slice() != ["INTEGER"] {
                return Err(format!("{}: only value or VAR INTEGER parameters are supported",proc.name));
            }
            for (name,_) in &section.names {
                parameters.push(name.clone());
                parameter_by_ref.push(section.by_ref);
            }
        }
        let mut locals = Vec::new();
        let mut fixed_arrays = Vec::new();
        let mut pointer_locals=Vec::<PointerObject>::new();
        let mut local_array_dims=HashMap::<String,Vec<u32>>::new();
        let mut local_record_fields=HashMap::<String,RecordShape>::new();
        for decl in &proc.decls.vars {
            if let Some(record)=pointer_record_target(
                &decl.ty,&pointer_types,&record_nominals)? {
                for (name,_) in &decl.names {
                    pointer_locals.push(checked_pointer_object(
                        name,&record,&checked_layouts,&record_types)?);
                }
                continue;
            }
            if let Some(fields) = record_fields(&decl.ty,&record_types,&pointer_types)? {
                let first = decl.names.first().ok_or("local record declaration has no names")?;
                let identity = format!("{}.{}.local.{}", module.name, proc.name, first.0);
                let record = checked_record_layout(
                    &decl.ty,&identity,&record_nominals,&pointer_types,&mut checked_layouts,
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
                TypeExpr::Array{dims,elem,..} if
                    matches!(elem.as_ref(),TypeExpr::Named(q,_) if q.as_slice()==["INTEGER"]) => {
                    let (lengths,length)=checked_array_dimensions(dims,&proc_constants)
                        .map_err(|err|format!("{}: {err}",proc.name))?;
                    for (name,_) in &decl.names {
                        fixed_arrays.push(ArrayLocal{name:name.clone(),length});
                        local_array_dims.insert(name.clone(),lengths.clone());
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
        let mut arrays:ArrayBindings<'_>=global_arrays.iter().enumerate()
            .map(|(i,a)|(a.name.as_str(),(fixed_arrays.len()+i,
                global_array_dims.get(&a.name).cloned().unwrap_or_else(||vec![a.length]))))
            .collect();
        for (i, array) in fixed_arrays.iter().enumerate() {
            arrays.insert(array.name.as_str(),(i,
                local_array_dims.get(&array.name).cloned().unwrap_or_else(||vec![array.length])));
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
        let mut pointers:PointerBindings=pointer_globals.iter()
            .enumerate().map(|(i,p)|(p.name.clone(),
                (pointer_locals.len()+i,p.clone()))).collect();
        for name in parameters.iter().chain(locals.iter())
            .chain(fixed_arrays.iter().map(|a|&a.name)) {
            pointers.remove(name);
        }
        for (i,p) in pointer_locals.iter().enumerate() {
            pointers.insert(p.name.clone(),(i,p.clone()));
        }
        let pointer_context=PointerContext {
            objects:&pointers,targets:&pointer_targets,
        };
        let body=lower_stmts(&proc.body,&names,&arrays,&records,&pointer_context,
            &signatures,&proc_constants)?;
        // An absent return is rejected rather than relying on Oberon's
        // zero-valued return accumulator (not modeled by this narrow bridge).
        if !definitely_returns(&body) {
            return Err(format!("{}: function must return a value on every path", proc.name));
        }
        functions.push(Function { name: proc.name.clone(), parameters,
                                  parameter_by_ref,locals,
                                  arrays:fixed_arrays,
                                  global_arrays:global_arrays.clone(),
                                  pointer_locals,pointer_globals:pointer_globals.clone(),
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
        let arrays:ArrayBindings<'_>=global_arrays.iter().enumerate()
            .map(|(i,item)|(item.name.as_str(),(i,
                global_array_dims.get(&item.name).cloned().unwrap_or_else(||vec![item.length]))))
            .collect();
        let records:RecordObjects=global_arrays.iter().enumerate()
            .filter_map(|(i,a)|module_record_fields.get(&a.name).map(|shape|
                (a.name.clone(),(i,shape.clone()))))
            .collect();
        let pointers:PointerBindings=pointer_globals.iter().enumerate()
            .map(|(i,p)|(p.name.clone(),(i,p.clone()))).collect();
        let pointer_context=PointerContext {
            objects:&pointers,targets:&pointer_targets,
        };
        let body=lower_stmts(&module.body,&names,&arrays,&records,&pointer_context,
            &signatures,&module_constants)?;
        if contains_return(&body) {
            return Err("module initialization cannot return a value".into());
        }
        Some(Function {
            name: "__module_init".into(),
            parameters: Vec::new(),parameter_by_ref:Vec::new(),
            locals: Vec::new(), arrays: Vec::new(),
            global_arrays: global_arrays.clone(),
            pointer_locals:Vec::new(),pointer_globals:pointer_globals.clone(),
            globals: globals.iter().map(|g| g.name.clone()).collect(),
            body,
        })
    };
    if functions.is_empty() && globals.is_empty() && global_arrays.is_empty()
        && pointer_globals.is_empty() && initializer.is_none() {
        return Err("module has no supported procedures, globals or initialization".into());
    }
    Ok((
        Module { name: module.name, globals, global_arrays, pointer_globals,
            functions, imported_procedures, initializer },
        exported_procedures,
        checked_layouts,
        checked_objects,
    ))
}

/// Conservative structured return guarantee. WHILE may execute zero
/// times, while REPEAT is projected as an unconditional body followed by
/// WHILE. An IF guarantees a return only if every arm does.
fn definitely_returns(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::Return(_) => true,
        Stmt::If { then_body, else_body, .. } =>
            definitely_returns(then_body) && definitely_returns(else_body),
        _ => false,
    })
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
    fn native_checked_functions_must_return_on_all_paths() {
        let partial = "MODULE R; PROCEDURE P*(x: INTEGER): INTEGER;
            BEGIN IF x > 0 THEN RETURN 1 END END P; END R.";
        assert!(lower_module(partial).unwrap_err().contains("every path"));
        let loop_only = "MODULE R; PROCEDURE P*(x: INTEGER): INTEGER;
            BEGIN WHILE x > 0 DO RETURN x END END P; END R.";
        assert!(lower_module(loop_only).unwrap_err().contains("every path"));
        let full = "MODULE R; PROCEDURE P*(x: INTEGER): INTEGER;
            BEGIN IF x > 0 THEN RETURN 1 ELSE RETURN -1 END END P; END R.";
        assert!(lower_module(full).is_ok());
        let fallback = "MODULE R; PROCEDURE P*(x: INTEGER): INTEGER;
            BEGIN IF x > 0 THEN RETURN 1 END; RETURN 0 END P; END R.";
        assert!(lower_module(fallback).is_ok());
    }

    #[test]
    fn native_repeat_until_preserves_first_iteration() {
        let src = "MODULE R; PROCEDURE P*(x: INTEGER): INTEGER;
            VAR n: INTEGER;
            BEGIN n := x; REPEAT n := n - 1 UNTIL n = 0;
            RETURN n END P; END R.";
        let m = lower_module(src).unwrap();
        let body = &m.functions[0].body;
        assert_eq!(body.len(), 4);
        assert!(matches!(&body[1], Stmt::Assign { .. }));
        assert!(matches!(&body[2], Stmt::While {
            condition: Expr::Binary(Op::Eq, _, rhs), body: repeated,
        } if **rhs == Expr::Boolean(false) &&
             repeated.as_slice() == &body[1..2]));
    }

    #[test]
    fn native_front_integer_literals_reject_narrowing() {
        let bad = "MODULE R; PROCEDURE P*(x: INTEGER): INTEGER;
            BEGIN RETURN 4294967295 END P; END R.";
        assert!(lower_module(bad).is_err());
        let good = "MODULE R; PROCEDURE P*(x: INTEGER): INTEGER;
            BEGIN RETURN -2147483648 END P; END R.";
        assert!(lower_module(good).is_ok());
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
        let multi=lower_module("MODULE A; PROCEDURE T*(x: INTEGER): INTEGER;
            VAR a: ARRAY 2, 3 OF INTEGER;
            BEGIN a[1,2]:=x; RETURN a[1,2] END T; END A.").unwrap();
        assert_eq!(multi.functions[0].arrays[0].length,6);
        assert!(matches!(multi.functions[0].body[0],Stmt::AssignMultiArray{..}));
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
        for dim in ["0", "1025", "1024,2"] {
            let src=format!("MODULE Data; VAR values: ARRAY {dim} OF INTEGER; END Data.");
            assert!(lower_module(&src).is_err());
        }
    }

    #[test]
    fn unsupported_global_types_are_rejected() {
        let array="MODULE M; VAR a: ARRAY 3, 2 OF INTEGER;
            PROCEDURE R*(v: INTEGER): INTEGER;
            BEGIN a[2,1]:=v; RETURN a[2,1] END R; END M.";
        assert_eq!(lower_module(array).unwrap().global_arrays[0].length,6);
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
