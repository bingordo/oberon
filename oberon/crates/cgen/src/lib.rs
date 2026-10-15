//! The C back end (DESIGN §9): the emitter, the ABI layout table and the runtime
//! it emits alongside, plus the second, Cranelift back end (DESIGN §16). Both
//! consume the same checked module and produce a translation unit or an object.

pub mod cgen;
pub mod clif;
pub mod layout;
