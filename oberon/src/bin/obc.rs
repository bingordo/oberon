//! `obc` — the driver: source in, runnable program out.
//!
//! The library turns one module into C; this binary is what makes that a
//! compiler. It walks the import graph, emits every module it has source for,
//! accepts hand-written C for the rest, writes the shared runtime, generates the
//! loader that `main` needs, invokes the C compiler with the flags DESIGN §9.1
//! fixes, and can run the result.
//!
//! ```text
//! obc check  Foo.mod [-I DIR]...                  diagnostics only
//! obc emit   Foo.mod [--out DIR] [-I DIR]...      .c/.h/.sym per module
//! obc build  Foo.mod [-o EXE] [--out DIR] [-I DIR]... [--cc CC] [--cflags …]
//! obc run    Foo.mod [-I DIR]... [--command M.P] [--keep]
//! ```
//!
//! Resolution, per `IMPORT M`: `M.mod` on the search path is compiled
//! (recursively, depth first, so its interface exists before the importer is
//! checked); otherwise `M.c` is taken as a hand-written implementation in the
//! emitted shape, as `oberon/e2e/Texts.c` is. `M.sym` must be findable either
//! way, because that is what the importing module is checked against.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use oberon_front::{
    cgen, check_module_recovering, emit_c, emit_interface, parse_module, parse_module_recovering,
    parse_with_diagnostics, render_diagnostics, sym, Diagnostic,
};

/// The flags DESIGN §9.1 fixes: wrapping arithmetic, the base-as-first-member
/// punning, and warnings as errors so the emitted C has to be clean.
const CFLAGS: &str = "-std=c11 -O2 -fwrapv -fno-strict-aliasing -Wall -Wextra -Werror";

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Check,
    Emit,
    Build,
    Run,
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
}

/// One module in the link: source this build emitted, or C found on disk.
struct Unit {
    name: String,
    c: PathBuf,
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
         \x20 --keep            keep the temporary build directory"
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
            other => {
                eprintln!("obc: unknown option `{other}`");
                return None;
            }
        }
        i += 1;
    }
    Some(o)
}

fn build_and_maybe_run(o: &Opts) -> Result<ExitCode, String> {
    std::fs::create_dir_all(&o.out).map_err(|e| format!("{}: {e}", o.out.display()))?;

    // 1. Interfaces already on the search path (hand-written modules such as
    //    Texts and Oberon ship one with their C).
    let mut interfaces = find_interfaces(o);

    // 2. Emit the module and everything it imports, depth first.
    let mut units: Vec<Unit> = Vec::new();
    let mut done: HashSet<String> = HashSet::new();
    let mut stack: Vec<String> = Vec::new();
    let root_src = read(&o.root)?;
    let root = module_name(&root_src);
    emit_tree(o, &o.root, &root_src, &root, &mut units, &mut interfaces, &mut done, &mut stack)?;

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
    write(&o.out.join("obc_main.c"), &loader_source(&units, o.command.as_ref()))?;
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
    for u in &units {
        cmd.arg(&u.c);
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
    let (parsed, syntax) = parse_with_diagnostics(src);
    let parsed = match parsed {
        Some(m) => m,
        None => {
            // Nothing parsed: the syntax diagnostics are all there is to report,
            // and they are already located.
            print!("{}", render_diagnostics(&path.display().to_string(), src, &syntax));
            return Err(format!("{} does not parse", path.display()));
        }
    };
    let imports: Vec<String> = parsed.imports.iter().map(|i| i.module.clone()).collect();
    for m in imports {
        if done.contains(&m) {
            continue;
        }
        let m = m.clone();
        match find_file(o, &format!("{m}.mod")) {
            Some(mod_path) => {
                let text = read(&mod_path)?;
                emit_tree(o, &mod_path, &text, &m, units, interfaces, done, stack)?;
            }
            None => match find_file(o, &format!("{m}.c")) {
                Some(c) if matches!(o.mode, Mode::Build | Mode::Run) => {
                    // A hand-written module: its header has to be next to it in
                    // the emitted shape, and its interface is already loaded.
                    if !c.with_extension("h").exists() {
                        return Err(format!(
                            "`{}` has no `{m}.h` beside it: a hand-written module needs the \
                             emitted shape",
                            c.display()
                        ));
                    }
                    units.push(Unit {
                        name: m.clone(),
                        c,
                    });
                    done.insert(m);
                }
                Some(_) => {
                    // check/emit only need the interface, which is already loaded.
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

    // Now the imports' interfaces exist, so this module can be checked. The
    // parser recovers at statement level, so one run reports syntax errors *and*
    // type errors rather than stopping at the first of either.
    let (parsed, diags) = check_module_recovering(src, interfaces);
    if diags.iter().any(Diagnostic::is_error) {
        print!("{}", render_diagnostics(&path.display().to_string(), src, &diags));
        return Err(format!("`{name}` does not check"));
    }
    let _ = parsed;
    let (c, h) = emit_c(src, interfaces)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .map_err(|diags| {
            print!("{}", render_diagnostics(&path.display().to_string(), src, &diags));
            format!("`{name}` does not check")
        })?;
    write(&o.out.join(format!("{name}.c")), &c)?;
    write(&o.out.join(format!("{name}.h")), &h)?;
    if let Some(text) = emit_interface(src, interfaces) {
        write(&o.out.join(format!("{name}.sym")), &text)?;
        if let Ok(iface) = sym::parse(&text) {
            // Every module in the build sees this interface from now on.
            interfaces.retain(|i| i.module != iface.module);
            interfaces.push(iface);
        }
    }
    units.push(Unit {
        name: name.to_string(),
        c: o.out.join(format!("{name}.c")),
    });
    done.insert(name.to_string());
    stack.pop();
    Ok(())
}

fn write_runtime(o: &Opts) -> Result<(), String> {
    write(&o.out.join("oberon.h"), cgen::RUNTIME_HEADER)?;
    write(&o.out.join("oberon_runtime.c"), cgen::RUNTIME_SOURCE)
}

fn find_interfaces(o: &Opts) -> Vec<sym::Interface> {
    let mut found: Vec<sym::Interface> = Vec::new();
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
                if let Ok(i) = sym::parse(&text) {
                    if !found.iter().any(|f| f.module == i.module) {
                        found.push(i);
                    }
                }
            }
        }
    }
    found
}

fn find_file(o: &Opts, name: &str) -> Option<PathBuf> {
    o.search.iter().map(|d| d.join(name)).find(|p| p.exists())
}

/// The loader `main`: load the modules in dependency order. Each `<M>__load`
/// runs that module's body and registers its commands, which is exactly what
/// report §11 says happens when a module is loaded.
fn loader_source(units: &[Unit], command: Option<&(String, String)>) -> String {
    let mut out = String::from("/* Generated by obc: load the modules, in dependency order. */\n");
    out.push_str("#include <stdio.h>\n");
    for u in units {
        out.push_str(&format!("#include \"{}.h\"\n", u.name));
    }
    out.push_str("\nint main(void) {\n");
    for u in units {
        out.push_str(&format!("    {}__load();\n", u.name));
    }
    match command {
        // report §11: a parameterless exported procedure can be activated by
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
fn module_name(src: &str) -> String {
    match parse_with_diagnostics(src).0 {
        Some(m) => m.name,
        // Nothing parsed: name it after the file, so the message reads naturally.
        None => parse_module(src)
            .map(|m| m.name)
            .unwrap_or_else(|_| "the module".to_string()),
    }
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}
