//! The C back end (DESIGN 9): the emitter and the runtime it emits alongside.
//! The second back end is its own crate (`clif`, DESIGN 16), so a build that
//! only wants this one does not compile Cranelift.

pub mod cgen;
