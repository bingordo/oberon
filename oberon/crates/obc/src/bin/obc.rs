//! `obc` is the driver: it turns source into a runnable program.
//!
//! The library turns one module into C. This binary completes the compiler by
//! resolving imports, writing C modules and the shared runtime, and generating
//! the loader that `main` needs. It then invokes the C compiler with the flags
//! in DESIGN 9.1 and can run the result.
//!
//! ```text
//! obc check  Foo.mod [-I DIR]...                  diagnostics only
//! obc emit   Foo.mod [--out DIR] [-I DIR]...      .c/.h/.sym per module
//! obc build  Foo.mod [-o EXE] [--out DIR] [-I DIR]... [--cc CC] [--cflags ...]
//! obc run    Foo.mod [-I DIR]... [--command M.P] [--keep]
//! ```
//!
//! For `IMPORT M`, the compiler recursively compiles `M.mod` from the search
//! path, depth first, so its interface exists before it checks the importer.
//! Otherwise it uses `M.c` as hand-written code in the emitted shape, as in
//! `oberon/e2e/Texts.c`. In either case, `M.sym` must be findable
//! because the importer is checked against it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use obc::{
    check_module_recovering_dialect, emit_c_full, emit_c_full_with_line_directives,
    emit_interface_dialect, emit_object_full, layout_table_full, limit_errors,
    parse_with_diagnostics_dialect, render_diagnostics, Diagnostic, Dialect,
};
use sema::sym;

/// DESIGN 9.1 sets the C flags for wrapping arithmetic, base-as-first-member
/// punning, and warnings as errors. This keeps the emitted C clean.
const CFLAGS: &str =
    "-std=c11 -O2 -fwrapv -fno-strict-aliasing -Wall -Wextra -Werror -Wno-overflow";

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Check,
    Emit,
    Build,
    Run,
}

/// Selects the back end for modules with source in this build (PLAN.md W9.1).
/// `c` is the reference and default. `clif` emits an object file per module
/// instead of a translation unit and header. The driver links it the same way
/// (DESIGN 16.2).
#[derive(PartialEq, Clone, Copy)]
enum Backend {
    C,
    Clif,
}

impl Backend {
    fn from_name(name: &str) -> Option<Backend> {
        match name {
            "c" => Some(Backend::C),
            "clif" => Some(Backend::Clif),
            _ => None,
        }
    }
}

struct Opts {
    mode: Mode,
    root: PathBuf,
    out: PathBuf,
    out_given: bool,
    exe: Option<PathBuf>,
    search: Vec<PathBuf>,
    cc: String,
    cflags: String,
    /// Extra compiler flags, one argv entry each (no whitespace splitting).
    cflag: Vec<String>,
    command: Option<(String, String)>,
    keep: bool,
    /// Generate C preprocessor line directives for source statements.
    line_directives: bool,
    /// Which back end emits this build's modules.
    backend: Backend,
    /// Print the module's type layout and stop without emitting anything. The
    /// result supports the comparison in DESIGN 16.
    layout: bool,
    /// How many errors to print before saying the rest were suppressed; `0` is
    /// no limit (PLAN.md W5.2).
    ferror_limit: usize,
    /// Which runtime checks to write. All are enabled unless `-Ochecks=...`
    /// narrows the set (DESIGN 9.4, PLAN.md W3.1 and W8.3).
    checks: cgen::cgen::Checks,
    /// Which dialect to accept: the report's Oberon-07 unless `-Odialect=lower`
    /// (DESIGN 5, PLAN.md W8.10).
    dialect: Dialect,
    /// What the second back end optimises for, in Cranelift's vocabulary: `speed`
    /// unless `-Oopt=...` says otherwise (DESIGN 16, PLAN.md W9.7). The C back
    /// end's level is `gcc`'s business and is chosen with `--cflag -O...`, so this
    /// only ever reaches the Cranelift back end.
    opt: String,
}

/// A module in the link, either code emitted by this build or C found on disk.
/// Its format depends on the back end: a translation unit for C or an object
/// file for Cranelift. The C compiler takes either format on its command line,
/// so both fit in one field.
struct Unit {
    name: String,
    code: PathBuf,
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: obc <check|emit|build|run> FILE.mod [options]\n\
         \n\
         options:\n\
         \x20 -I DIR            interface/source search path (repeatable)\n\
         \x20 --out DIR         where to write generated files (default: `.` for emit/check,\n\
         \x20                   a temporary directory for build/run)\n\
         \x20 -o EXE            output executable (build; default ./<Module>)\n\
         \x20 --cc CC           C compiler (default: $CC or cc)\n\
         \x20 --cflags \"…\"      extra C flags (split on whitespace)\n\
         \x20 --cflag ARG       one extra C flag (repeatable, no splitting)\n\
         \x20 --command M.P     run this command after the modules load (run)\n\
         \x20 --keep            keep the temporary build directory\n\
         \x20 --line-directives  map generated C statements to their Oberon source lines\n\
         \x20 --ferror-limit N  print at most N errors (default 20, 0 = no limit)
         \x20 -Ochecks[=LIST]   runtime checks: all (default), none, or a list of
         \x20                   index, nil, guard and case
         \x20 -Odialect=NAME    report (default) or lower, which folds reserved-word case\n\
         \x20 -Oopt=LEVEL      the second back end's optimisation: speed (default),\n\
         \x20                  none for the fastest compiles, or speed_and_size\n\
         \x20 --backend=NAME    c (default) writes C, clif writes an object file per\n\
         \x20                   module with the Cranelift back end (PLAN.md W9.1)\n\
         \x20 --layout          print the C ABI's layout of the module's types and stop\n\
         \x20                   (DESIGN §16; the second back end computes it, `cc` is the\n\
         \x20                   authority, and `e2e` compares the two)"
    );
    ExitCode::FAILURE
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut opts = match parse_args(&args) {
        Some(o) => o,
        None => return usage(),
    };
    if opts.search.is_empty() {
        match opts.root.parent() {
            Some(p) if !p.as_os_str().is_empty() => opts.search.push(p.to_path_buf()),
            _ => opts.search.push(PathBuf::from(".")),
        }
    }
    // Only a build/run needs a scratch directory; `emit` writes where the user
    // asked (default `.`) and `check` writes nothing at all.
    let temporary = !opts.out_given && matches!(opts.mode, Mode::Build | Mode::Run);
    if temporary {
        opts.out = std::env::temp_dir().join(format!("obc-{}", std::process::id()));
    }
    // The build directory also holds interfaces. The runtime writes `Args.sym`
    // there, letting a program reach the host through an ordinary module
    // without an added search path. Since this directory is appended last,
    // `-I` entries still shadow it (DESIGN 11).
    if !opts.search.contains(&opts.out) {
        opts.search.push(opts.out.clone());
    }
    let code = match build_and_maybe_run(&opts) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("obc: {e}");
            ExitCode::FAILURE
        }
    };
    if temporary && !opts.keep {
        let _ = std::fs::remove_dir_all(&opts.out);
    }
    code
}

fn parse_args(args: &[String]) -> Option<Opts> {
    let mode = match args.first()?.as_str() {
        "check" => Mode::Check,
        "emit" => Mode::Emit,
        "build" => Mode::Build,
        "run" => Mode::Run,
        _ => return None,
    };
    let root = PathBuf::from(args.get(1)?);
    let mut o = Opts {
        mode,
        root,
        out: PathBuf::from("."),
        out_given: false,
        exe: None,
        search: Vec::new(),
        cc: std::env::var("CC").unwrap_or_else(|_| "cc".to_string()),
        cflags: String::new(),
        cflag: Vec::new(),
        command: None,
        keep: false,
        backend: Backend::C,
        layout: false,
        ferror_limit: 20,
        checks: cgen::cgen::Checks::ALL,
        dialect: Dialect::Report,
        opt: "speed".to_string(),
        line_directives: false,
    };
    let mut i = 2;
    while i < args.len() {
        let a = args[i].clone();
        let take = |i: &mut usize| -> Option<String> {
            *i += 1;
            args.get(*i).cloned()
        };
        match a.as_str() {
            "-I" => o.search.push(PathBuf::from(take(&mut i)?)),
            "--out" => {
                o.out = PathBuf::from(take(&mut i)?);
                o.out_given = true;
            }
            "-o" => o.exe = Some(PathBuf::from(take(&mut i)?)),
            "--cc" => o.cc = take(&mut i)?,
            "--cflags" => o.cflags = take(&mut i)?,
            "--cflag" => o.cflag.push(take(&mut i)?),
            "--command" => {
                let spec = take(&mut i)?;
                let (m, p) = spec.split_once('.')?;
                o.command = Some((m.to_string(), p.to_string()));
            }
            "--keep" => o.keep = true,
            "--line-directives" => o.line_directives = true,
            "--layout" => o.layout = true,
            other if other.starts_with("--backend=") => {
                let name = &other["--backend=".len()..];
                match Backend::from_name(name) {
                    Some(b) => o.backend = b,
                    None => {
                        eprintln!(
                            "obc: `--backend={name}` is not a back end this compiler has; \
                             expected c or clif"
                        );
                        return None;
                    }
                }
            }
            other if other.starts_with("-Odialect=") => {
                let name = &other["-Odialect=".len()..];
                match Dialect::from_name(name) {
                    Some(d) => o.dialect = d,
                    None => {
                        eprintln!(
                            "obc: `-Odialect={name}` is not a dialect this compiler knows; \
                             expected report or lower"
                        );
                        return None;
                    }
                }
            }
            other if other.starts_with("-Oopt=") => {
                let name = &other["-Oopt=".len()..];
                if !matches!(name, "none" | "speed" | "speed_and_size" | "speed-and-size") {
                    eprintln!(
                        "obc: `-Oopt={name}` names no optimisation level; \
                         expected none, speed or speed_and_size"
                    );
                    return None;
                }
                o.opt = name.to_string();
            }
            "-Ochecks" => o.checks = cgen::cgen::Checks::ALL,
            other if other.starts_with("-Ochecks=") => {
                let list = &other["-Ochecks=".len()..];
                match cgen::cgen::Checks::parse(list) {
                    Some(c) => o.checks = c,
                    None => {
                        eprintln!(
                            "obc: `-Ochecks={list}` names no check; expected one of {}",
                            cgen::cgen::Checks::NAMES
                        );
                        return None;
                    }
                }
            }
            "--ferror-limit" => {
                o.ferror_limit = take(&mut i)?
                    .parse()
                    .map_err(|_| "--ferror-limit wants a number (0 means no limit)".to_string())
                    .ok()?;
            }
            other => {
                eprintln!("obc: unknown option `{other}`");
                return None;
            }
        }
        i += 1;
    }
    if o.line_directives && (o.mode == Mode::Check || o.backend != Backend::C) {
        eprintln!("obc: --line-directives requires C output (emit, build, or run with --backend=c)");
        return None;
    }
    Some(o)
}

fn build_and_maybe_run(o: &Opts) -> Result<ExitCode, String> {
    std::fs::create_dir_all(&o.out).map_err(|e| format!("{}: {e}", o.out.display()))?;

    // 1. Interfaces already on the search path (hand-written modules such as
    //    Texts and Oberon include one with their C).
    // An interface that is *refused* is not the same as one that is missing, but
    // saying so has to wait until we know it is needed: an unrelated `.sym` on the
    // search path, or one superseded by a better copy in another directory, must
    // not be reported (PLAN.md W3.2). The reasons are kept here and printed when
    // the check says the interface is missing.
    // The runtime's interface is written before anything is resolved: `Args.sym`
    // lives with it, so a program that imports the host's arguments finds it on
    // the build directory's own search path (DESIGN 11).
    write_runtime(o)?;
    let (mut interfaces, refused) = find_interfaces(o);

    // 2. Emit the module and everything it imports, depth first.
    let mut units: Vec<Unit> = Vec::new();
    let mut done: HashSet<String> = HashSet::new();
    let mut stack: Vec<String> = Vec::new();
    let root_src = read(&o.root)?;
    let root = module_name(&root_src, o.dialect);
    if o.layout {
        // The table describes types, so it can report layouts for records or
        // pointers this back end does not yet lower. This supports comparison
        // against `cc` and stops before building anything.
        let pointer_bytes = clif::clif::pointer_bytes().map_err(|e| format!("--layout: {e}"))?;
        match layout_table_full(&root_src, &interfaces, o.dialect, pointer_bytes) {
            Ok(Ok(table)) => print!("{table}"),
            Ok(Err(diags)) => {
                print!("{}", render_diagnostics(&o.root.display().to_string(), &root_src, &diags));
                return Err(format!("`{root}` does not check"));
            }
            Err(e) => return Err(format!("{}: {e}", o.root.display())),
        }
        return Ok(ExitCode::SUCCESS);
    }
    emit_tree(
        o,
        &refused,
        &o.root,
        &root_src,
        &root,
        &mut units,
        &mut interfaces,
        &mut done,
        &mut stack,
    )?;

    if o.mode == Mode::Check {
        // Diagnostics only: nothing is written, not even the runtime.
        for name in &done {
            let _ = name;
        }
        return Ok(ExitCode::SUCCESS);
    }
    if o.mode == Mode::Emit {
        write_runtime(o)?;
        eprintln!("obc: wrote {} module(s) into {}", units.len(), o.out.display());
        return Ok(ExitCode::SUCCESS);
    }

    // 3. Runtime, loader, and the C compiler.
    write_runtime(o)?;
    write(
        &o.out.join("obc_main.c"),
        &loader_source(&units, o.command.as_ref(), o.backend),
    )?;
    let exe = o.exe.clone().unwrap_or_else(|| {
        if o.mode == Mode::Build {
            PathBuf::from(format!("./{root}"))
        } else {
            o.out.join(&root)
        }
    });
    let mut cmd = Command::new(&o.cc);
    cmd.args(CFLAGS.split_whitespace());
    cmd.args(o.cflags.split_whitespace());
    for f in &o.cflag {
        cmd.arg(f);
    }
    cmd.arg("-I").arg(&o.out);
    for dir in &o.search {
        cmd.arg("-I").arg(dir);
    }
    // A unit is a `.c` or a `.o`; the compiler driver takes either, which is
    // what makes the back ends interchangeable here (DESIGN 16.2).
    for u in &units {
        cmd.arg(&u.code);
    }
    cmd.arg(o.out.join("oberon_runtime.c"));
    cmd.arg(o.out.join("obc_main.c"));
    cmd.arg("-o").arg(&exe);
    eprintln!("obc: linking {} module(s) with {}", units.len(), o.cc);
    let out = cmd.output().map_err(|e| format!("cannot run `{}`: {e}", o.cc))?;
    if !out.status.success() {
        eprint!("{}", String::from_utf8_lossy(&out.stderr));
        return Err(format!("the C compiler failed (wanted {})", exe.display()));
    }
    if o.mode == Mode::Build {
        eprintln!("obc: wrote {}", exe.display());
        return Ok(ExitCode::SUCCESS);
    }

    // 4. Run: every module's body has already run by the time loading ends, so
    //    only an explicitly requested command is dispatched here.
    let mut run = Command::new(&exe);
    let status = run.status().map_err(|e| format!("cannot run {}: {e}", exe.display()))?;
    Ok(match status.code() {
        Some(0) => ExitCode::SUCCESS,
        Some(c) => ExitCode::from(c as u8),
        None => ExitCode::FAILURE,
    })
}

#[allow(clippy::too_many_arguments)]
fn emit_tree(
    o: &Opts,
    refused: &HashMap<String, String>,
    path: &Path,
    src: &str,
    name: &str,
    units: &mut Vec<Unit>,
    interfaces: &mut Vec<sym::Interface>,
    done: &mut HashSet<String>,
    stack: &mut Vec<String>,
) -> Result<(), String> {
    if done.contains(name) {
        return Ok(());
    }
    if stack.iter().any(|s| s == name) {
        return Err(format!("import cycle through `{name}`"));
    }
    stack.push(name.to_string());

    // Parse with recovery: the imports are needed to resolve dependencies even
    // when the module has syntax errors, and the errors themselves are reported
    // below together with the semantic ones.
    let (parsed, syntax) = parse_with_diagnostics_dialect(src, o.dialect);
    let parsed = match parsed {
        Some(m) => m,
        None => {
            // Nothing parsed: the syntax diagnostics are all there is to report,
            // and they are already located.
            let mut syntax = syntax;
            let suppressed = limit_errors(&mut syntax, o.ferror_limit);
            print!("{}", render_diagnostics(&path.display().to_string(), src, &syntax));
            if suppressed > 0 {
                println!(
                    "{suppressed} more error(s) were suppressed (--ferror-limit {})",
                    o.ferror_limit
                );
            }
            return Err(format!("{} does not parse", path.display()));
        }
    };
    let imports: Vec<String> = parsed.imports.iter().map(|i| i.module.clone()).collect();
    for m in imports {
        if done.contains(&m) {
            continue;
        }
        // DESIGN 9.10 defines SYSTEM in report 11.1 as built into the compiler.
        // There is no module to compile, link, or search for. Looking for
        // `SYSTEM.mod` could shadow the report's module with one on the search
        // path, or fail the build if the path has no such module.
        if m == "SYSTEM" {
            continue;
        }
        let m = m.clone();
        match find_file(o, &format!("{m}.mod")) {
            Some(mod_path) => {
                let text = read(&mod_path)?;
                emit_tree(o, refused, &mod_path, &text, &m, units, interfaces, done, stack)?;
            }
            None => match find_file(o, &format!("{m}.c")) {
                Some(c) if matches!(o.mode, Mode::Build | Mode::Run) => {
                    // A hand-written module needs its header beside it in the
                    // emitted shape. Its interface is already loaded.
                    if !c.with_extension("h").exists() {
                        return Err(format!(
                            "`{}` has no `{m}.h` beside it: a hand-written module needs the \
                             emitted shape",
                            c.display()
                        ));
                    }
                    units.push(Unit {
                        name: m.clone(),
                        code: c,
                    });
                    done.insert(m);
                }
                Some(_) => {
                    // check/emit only need the interface, which is already loaded.
                    done.insert(m);
                }
                None if is_runtime_module(&m) => {
                    // Its body is in the runtime, which every build links: there is
                    // nothing to emit and nothing to find on disk (DESIGN 11).
                    done.insert(m);
                }
                None if matches!(o.mode, Mode::Build | Mode::Run) => {
                    return Err(format!(
                        "no implementation for module `{m}`: looked for {m}.mod and {m}.c on the \
                         search path"
                    ))
                }
                None => {
                    if !interfaces.iter().any(|i| i.module == m) {
                        // A refused interface has an unsupported version. It
                        // is not missing, and identifying it avoids blaming an
                        // unrelated `.sym` on the search path (PLAN.md W3.2).
                        //
                        // If the module's C exists, this arm is not reached;
                        // `emit_tree` reports the refusal in the checker's
                        // diagnostic instead. Only one path can report it.
                        if let Some(why) = refused.get(&m) {
                            return Err(why.to_string());
                        }
                        return Err(format!(
                            "no interface for module `{m}` on the search path (looked for {m}.sym, \
                             {m}.mod and {m}.c)"
                        ));
                    }
                    done.insert(m);
                }
            },
        }
    }

    // The imports' interfaces are ready, so this module can be checked. The
    // parser recovers at statement level and reports syntax and type errors
    // together rather than stopping at the first one.
    let (parsed, mut diags) = check_module_recovering_dialect(src, interfaces, o.dialect);
    // A missing-interface diagnostic may mean the interface was refused
    // because this compiler does not read its version. This point knows an
    // interface is needed, whether it comes from `.sym`, `.mod`, or C
    // (PLAN.md W3.2).
    for d in &diags {
        if let Some(rest) = d.message.split("no interface for module `").nth(1) {
            if let Some(name) = rest.split('`').next() {
                if let Some(why) = refused.get(name) {
                    eprintln!("{why}");
                }
            }
        }
    }
    let suppressed = limit_errors(&mut diags, o.ferror_limit);
    print!("{}", render_diagnostics(&path.display().to_string(), src, &diags));
    if suppressed > 0 {
        println!(
            "{suppressed} more error(s) were suppressed (--ferror-limit {})",
            o.ferror_limit
        );
    }
    if diags.iter().any(Diagnostic::is_error) {
        return Err(format!("`{name}` does not check"));
    }
    let _ = parsed;
    // The back ends write different formats. C writes a translation unit and
    // header. Cranelift writes an object file. Both use the same runtime and
    // loader (DESIGN 16.2), so the rest of the driver is independent of the
    // selected back end.
    let code = match o.backend {
        Backend::C => {
            let emitted = if o.line_directives {
                emit_c_full_with_line_directives(
                    src,
                    interfaces,
                    o.checks,
                    o.dialect,
                    &path.display().to_string(),
                )
            } else {
                emit_c_full(src, interfaces, o.checks, o.dialect)
            };
            let (c, h) = emitted
                .map_err(|e| format!("{}: {e}", path.display()))?
                .map_err(|diags| {
                    print!("{}", render_diagnostics(&path.display().to_string(), src, &diags));
                    format!("`{name}` does not check")
                })?;
            write(&o.out.join(format!("{name}.c")), &c)?;
            write(&o.out.join(format!("{name}.h")), &h)?;
            o.out.join(format!("{name}.c"))
        }
        Backend::Clif => {
            let object = emit_object_full(src, interfaces, o.checks, o.dialect, &o.opt)
                .map_err(|e| format!("{}: {e}", path.display()))?
                .map_err(|diags| {
                    print!("{}", render_diagnostics(&path.display().to_string(), src, &diags));
                    format!("`{name}` was not lowered by the clif back end")
                })?;
            write_bytes(&o.out.join(format!("{name}.o")), &object)?;
            o.out.join(format!("{name}.o"))
        }
    };
    if let Some(text) = emit_interface_dialect(src, interfaces, o.dialect) {
        write(&o.out.join(format!("{name}.sym")), &text)?;
        if let Ok(iface) = sym::parse(&text) {
            // Every module in the build sees this interface from now on.
            interfaces.retain(|i| i.module != iface.module);
            interfaces.push(iface);
        }
    }
    units.push(Unit {
        name: name.to_string(),
        code,
    });
    done.insert(name.to_string());
    stack.pop();
    Ok(())
}

/// The `Args` module's header, in the shape the emitter writes for an imported
/// module: the declaration a caller compiles against. The body is in the runtime,
/// so this file is all the driver has to add to the pair every module has.
const ARGS_HEADER: &str = "\
/* The Args module, whose body is in the runtime (DESIGN 11). */
#ifndef ARGS_H
#define ARGS_H
#include \"oberon.h\"
int32_t Args__Count(void);
void Args__Get(int32_t i, char *s, int32_t s__len);
#endif
";

/// The runtime provides these module bodies instead of finding a `.c` on the
/// search path. Programs use them like ordinary modules, through imports,
/// interfaces, and calls that follow the usual ABI. The driver only needs to
/// know that their code is already in the object linked by every build. Add a
/// row for each host-provided module, and put its C in the runtime (DESIGN 11).
const RUNTIME_MODULES: &[&str] = &["Args"];

fn is_runtime_module(name: &str) -> bool {
    RUNTIME_MODULES.contains(&name)
}

/// The `Args` interface uses the `.sym` shape written by the emitter. This
/// driver writes it because the module body is in the runtime, with no separate
/// declaration to emit. `Count` returns the argument count excluding the
/// program name. `Get` copies argument `i` into a string, or an empty string
/// when none exists (DESIGN 11).
const ARGS_INTERFACE: &str = "\
MODULE Args
VERSION 1
PROC Count(): INTEGER
PROC Get(i: INTEGER; VAR s: ARRAY OF CHAR)
";

fn write_runtime(o: &Opts) -> Result<(), String> {
    write(&o.out.join("oberon.h"), cgen::cgen::RUNTIME_HEADER)?;
    // `Args` is an ordinary module with its body in the runtime. Write its
    // interface beside the body. Programs import it to read arguments; the
    // compiler does not know its name (DESIGN 11).
    write(&o.out.join("Args.sym"), ARGS_INTERFACE)?;
    write(&o.out.join("Args.h"), ARGS_HEADER)?;
    write(&o.out.join("oberon_runtime.c"), cgen::cgen::RUNTIME_SOURCE)
}

fn find_interfaces(o: &Opts) -> (Vec<sym::Interface>, HashMap<String, String>) {
    let mut found: Vec<sym::Interface> = Vec::new();
    // An interface the loader cannot read is not the same as a missing one: it
    // exists, and saying *why* it was refused (a version this compiler does not
    // read, say) is the difference between "recompile that module" and "there is
    // no such module" (PLAN.md W3.2).
    let mut refused: HashMap<String, String> = HashMap::new();
    for dir in &o.search {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("sym") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&p) {
                match sym::parse(&text) {
                    Ok(i) => {
                        if !found.iter().any(|f| f.module == i.module) {
                            found.push(i);
                        }
                    }
                    Err(e) => {
                        // Keyed by the module its own header names, because that is
                        // what a missing interface is reported as: a hand-renamed
                        // `Alias.sym` holding `MODULE M` must be found under `M`.
                        // The header is readable even when the rest is not.
                        let named = text
                            .lines()
                            .find_map(|l| l.trim().strip_prefix("MODULE "))
                            .and_then(|rest| rest.split_whitespace().next())
                            .map(str::to_string);
                        let key = named.or_else(|| {
                            p.file_stem().and_then(|s| s.to_str()).map(str::to_string)
                        });
                        if let Some(key) = key {
                            refused
                                .entry(key)
                                .or_insert_with(|| format!("{}: {e}", p.display()));
                        }
                    }
                }
            }
        }
    }
    (found, refused)
}

fn find_file(o: &Opts, name: &str) -> Option<PathBuf> {
    o.search.iter().map(|d| d.join(name)).find(|p| p.exists())
}

/// The loader `main` loads modules in dependency order. Each `<M>__load` runs
/// that module's body and registers its commands, as report 11 describes.
///
/// Both back ends use the same `main` with different prologues. C modules
/// have declarations in a header, which the loader includes. Cranelift modules
/// have no header; their object and `.sym` hold the interface, so the loader
/// declares the entry point it calls. In either case, command dispatch uses
/// the runtime's registry, not module names (DESIGN 16.2).
fn loader_source(
    units: &[Unit],
    command: Option<&(String, String)>,
    backend: Backend,
) -> String {
    let mut out = String::from("/* Generated by obc: load the modules, in dependency order. */\n");
    out.push_str("#include <stdio.h>\n");
    match backend {
        Backend::C => {
            for u in units {
                out.push_str(&format!("#include \"{}.h\"\n", u.name));
            }
        }
        Backend::Clif => {
            // `ob_run_command` is the runtime's, so the registry's declaration
            // is what this file includes.
            out.push_str("#include \"oberon.h\"\n");
            for u in units {
                out.push_str(&format!("void {}__load(void);\n", u.name));
            }
        }
    }
    // The loader's own first act is to hand the program's arguments to the
    // runtime, which is where the `Args` module reads them (DESIGN 11). It takes
    // them even when the program ignores them: one `main` shape for every build
    // is worth more than a special case per program.
    out.push_str("\nint main(int argc, char **argv) {\n    ob_set_args(argc, argv);\n");
    for u in units {
        out.push_str(&format!("    {}__load();\n", u.name));
    }
    match command {
        // report 11: a parameterless exported procedure can be activated by
        // name once its module is loaded, which is what `--command` does.
        Some((module, proc)) => {
            out.push_str(&format!(
                "    if (!ob_run_command(\"{module}\", \"{proc}\")) {{\n        \
                 fprintf(stderr, \"no command {module}.{proc}\\n\");\n        return 2;\n    }}\n"
            ));
            out.push_str("    return 0;\n}\n");
        }
        None => out.push_str("    return 0;\n}\n"),
    }
    out
}

/// The module's name, taken from the part that parsed: a syntax error later in
/// the file must not stop the driver from knowing which module it is looking at.
/// The module's name, from whatever parsed. A file too broken to parse still has
/// to reach the diagnostics, so this never fails: the name is only used for
/// output file names, which nothing reaches when the module does not parse.
fn module_name(src: &str, dialect: Dialect) -> String {
    let (parsed, _) = parse_with_diagnostics_dialect(src, dialect);
    match parsed {
        Some(m) => m.name,
        // Nothing parsed: name it after the file, so the message reads naturally.
        None => "the module".to_string(),
    }
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// The object a back end produced is bytes, not text.
fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}
