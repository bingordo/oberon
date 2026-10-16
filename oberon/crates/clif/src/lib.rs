//! The Cranelift back end (DESIGN §16): the whole-language lowering, and the ABI
//! layout table it checks itself against. Both consume the same checked module
//! `cgen` does and produce an object rather than a translation unit.

pub mod clif;
pub mod layout;

