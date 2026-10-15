//! The semantic phase: name resolution, the type algebra, the constant folder
//! and the checks of DESIGN §7–§8. It consumes the syntax AST and produces a
//! checked module plus its diagnostics; it never re-reads the source text.

pub mod const_eval;
pub mod resolve;
pub mod sema;
pub mod sym;
pub mod types;

pub use syntax::diag::{dedup_diagnostics, Diagnostic, Severity};

use syntax::ast::Module;
use syntax::diag::parse_with_diagnostics_dialect;
use syntax::{parse_module, Dialect, ParseErr};
use sym::Interface;

/// Parse *and* check a module, reporting syntax errors and semantic errors in
/// one run: the grammar recovers at statement level, so a file with three bad
/// statements is still type-checked.
pub fn check_module_recovering(
    input: &str,
    interfaces: &[Interface],
) -> (Option<Module>, Vec<Diagnostic>) {
    check_module_recovering_dialect(input, interfaces, Dialect::Report)
}

pub fn check_module_recovering_dialect(
    input: &str,
    interfaces: &[Interface],
    dialect: Dialect,
) -> (Option<Module>, Vec<Diagnostic>) {
    let (module, mut diags) = parse_with_diagnostics_dialect(input, dialect);
    match module {
        Some(mut m) => {
            let (_, mut sem) = sema::Sema::check(&mut m, interfaces);
            diags.append(&mut sem);
            dedup_diagnostics(&mut diags);
            diags.sort_by_key(|d| (d.span.lo, d.span.hi));
            (Some(m), diags)
        }
        None => (None, diags),
    }
}

/// Parse a module and run the semantic pass over it (DESIGN §7-§8). The module
/// comes back with its selectors reclassified, so tests can inspect both.
pub fn check_module<'input>(
    input: &'input str,
    interfaces: &[Interface],
) -> Result<(Module, Vec<Diagnostic>), ParseErr<'input>> {
    let mut m = parse_module(input)?;
    let (_, diags) = sema::Sema::check(&mut m, interfaces);
    Ok((m, diags))
}
