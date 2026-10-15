//! Driver: `oberon-front FILE.mod` parses and prints the per-procedure AST.
//! `oberon-front -` reads standard input.

use std::io::Read;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().map(String::as_str).unwrap_or("-");
    // The same error limit the driver has, so `--ferror-limit` means the same
    // thing in both CLIs (PLAN.md W5.2). 0 is no limit.
    let ferror_limit: usize = args
        .iter()
        .position(|a| a == "--ferror-limit")
        .and_then(|i| args.get(i + 1))
        .and_then(|n| n.parse().ok())
        .unwrap_or(20);
    // The dialect, spelled the way the driver spells it (PLAN.md W8.10): the
    // report's Oberon-07 unless asked for the case-folding one.
    let dialect = match args.iter().position(|a| a.starts_with("-Odialect=")) {
        Some(i) => {
            let name = &args[i]["-Odialect=".len()..];
            match obc::Dialect::from_name(name) {
                Some(d) => d,
                None => {
                    eprintln!(
                        "`-Odialect={name}` is not a dialect this compiler knows; expected \
                         report or lower"
                    );
                    return ExitCode::FAILURE;
                }
            }
        }
        None => obc::Dialect::Report,
    };
    let report = |src: &str, diags: &mut Vec<obc::Diagnostic>| {
        let suppressed = obc::limit_errors(diags, ferror_limit);
        let text = obc::render_diagnostics(path, src, diags);
        if suppressed > 0 {
            println!("{text}{suppressed} more error(s) were suppressed (--ferror-limit {ferror_limit})");
        } else {
            print!("{text}");
        }
    };
    let src = if path == "-" {
        let mut buf = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut buf) {
            eprintln!("error: cannot read stdin: {e}");
            return ExitCode::FAILURE;
        }
        buf
    } else {
        match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: cannot read {path}: {e}");
                return ExitCode::FAILURE;
            }
        }
    };

    // `--emit-c DIR`: check the module and write DIR/<Name>.c, DIR/<Name>.h plus
    // the runtime pair.
    if let Some(i) = args.iter().position(|a| a == "--emit-c") {
        let dir = args.get(i + 1).cloned().unwrap_or_else(|| ".".to_string());
        // Interface files of the modules this one imports: DESIGN §7 has the
        // compiler write them, and the reader looks for them next to the source
        // and in each `-I DIR`.
        let mut ifaces = Vec::new();
        let mut search: Vec<std::path::PathBuf> = Vec::new();
        match std::path::Path::new(path).parent() {
            // A bare file name has an empty parent, which means "here".
            Some(p) if p.as_os_str().is_empty() => search.push(std::path::PathBuf::from(".")),
            Some(p) => search.push(p.to_path_buf()),
            None => search.push(std::path::PathBuf::from(".")),
        }
        for (k, a) in args.iter().enumerate() {
            if a == "-I" {
                if let Some(d) = args.get(k + 1) {
                    search.push(std::path::PathBuf::from(d));
                }
            }
        }
        for d in &search {
            if let Ok(entries) = std::fs::read_dir(d) {
                for ent in entries.flatten() {
                    let p = ent.path();
                    if p.extension().and_then(|e| e.to_str()) == Some("sym") {
                        if let Ok(text) = std::fs::read_to_string(&p) {
                            match sema::sym::parse(&text) {
                                Ok(i) => if !ifaces.iter().any(|x: &sema::sym::Interface| x.module == i.module) {
                                    ifaces.push(i);
                                },
                                Err(e) => eprintln!("warning: {}: {e}", p.display()),
                            }
                        }
                    }
                }
            }
        }
        return match obc::emit_c_full(
            &src,
            &ifaces,
            cgen::cgen::Checks::ALL,
            dialect,
        ) {
            Ok(Ok((c, h))) => {
                let name = obc::parse_module_dialect(&src, dialect)
                    .map(|m| m.name)
                    .unwrap_or_else(|_| "module".to_string());
                let write = |file: String, text: &str| -> std::io::Result<()> {
                    std::fs::write(std::path::Path::new(&dir).join(file), text)
                };
                let sym_text = obc::emit_interface_dialect(&src, &ifaces, dialect);
                let r = write(format!("{name}.c"), &c)
                    .and_then(|_| write(format!("{name}.h"), &h))
                    .and_then(|_| write("oberon.h".into(), cgen::cgen::RUNTIME_HEADER))
                    .and_then(|_| {
                        write("oberon_runtime.c".into(), cgen::cgen::RUNTIME_SOURCE)
                    })
                    .and_then(|_| match &sym_text {
                        Some(t) => write(format!("{name}.sym"), t),
                        None => Ok(()),
                    });
                match r {
                    Ok(()) => {
                        println!("wrote {dir}/{name}.c, {dir}/{name}.h, runtime");
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("error: cannot write into {dir}: {e}");
                        ExitCode::FAILURE
                    }
                }
            }
            Ok(Err(mut diags)) => {
                report(&src, &mut diags);
                ExitCode::FAILURE
            }
            Err(e) => {
                eprintln!("{path}: {e}");
                ExitCode::FAILURE
            }
        };
    }

    let check = args.iter().any(|a| a == "-c" || a == "--check");
    if check {
        let (_, mut diags) =
            obc::check_module_recovering_dialect(&src, &[], dialect);
        let has_error = diags.iter().any(|d| d.is_error());
        report(&src, &mut diags);
        return if has_error {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        };
    }
    match obc::parse_module_dialect(&src, dialect) {
        Ok(m) => {
            println!("{m:#?}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            // The same shape as any other diagnostic (DESIGN §10).
            let d = obc::diagnostic_for_parse_error(&e);
            eprint!("{}", obc::render_diagnostics(path, &src, &[d]));
            ExitCode::FAILURE
        }
    }
}
