//! Prototype of the interface-file (`*.sym`) format of DESIGN.md §7.
//!
//! Only the *format* is prototyped here: rendering a module's exported surface
//! and parsing it back. Resolution of the parsed declarations is the real
//! resolver's job. The format is text, one declaration per line, because it has
//! to be diffable and greppable (an interface change should show up in a code
//! review), and because writing a parser for it is ten lines rather than a
//! binary-format compatibility exercise.
//!
//! Grammar of the format (whitespace-normalised, one item per line):
//!
//! ```text
//! MODULE <name>
//! IMPORT <module> {"," <module>}
//! CONST <name> = <value>
//! TYPE <name> = <rendered type>
//! VAR <name>: <rendered type>        (* read-only for importers, report §11 *)
//! PROC <name>(<type>; VAR <type>; ARRAY OF <type>): <result>
//! ```
//!
//! Everything after `=` / `:` / `(` is opaque text in this prototype: it is the
//! *rendered* type, which the real resolver parses with the same type parser the
//! grammar uses. The prototype checks the part that must not be ambiguous — the
//! line structure and the declaration kinds — by round-tripping.

use std::fmt::Write as _;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Const { name: String, value: String },
    Type { name: String, def: String },
    /// Exported variables are read-only for importers (report §11).
    Var { name: String, ty: String },
    Proc {
        name: String,
        /// One entry per parameter, already mode-prefixed (`VAR T`, `ARRAY OF T`).
        params: Vec<String>,
        result: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Interface {
    pub module: String,
    /// The modules this interface's own type renderings refer to. An interface
    /// has to name them, otherwise `VAR Log: Texts.Writer` in module `Oberon`
    /// could not be resolved by a client that imports `Oberon` but not `Texts`
    /// (DESIGN §7).
    pub imports: Vec<String>,
    pub items: Vec<Item>,
}

pub fn render(i: &Interface) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "MODULE {}", i.module);
    if !i.imports.is_empty() {
        let _ = writeln!(s, "IMPORT {}", i.imports.join(", "));
    }
    for it in &i.items {
        match it {
            Item::Const { name, value } => {
                let _ = writeln!(s, "CONST {name} = {value}");
            }
            Item::Type { name, def } => {
                let _ = writeln!(s, "TYPE {name} = {def}");
            }
            Item::Var { name, ty } => {
                let _ = writeln!(s, "VAR {name}: {ty}");
            }
            Item::Proc {
                name,
                params,
                result,
            } => {
                // A parameterless proper procedure is written without
                // parentheses (`PROC WriteLn`), which is also how the report
                // lets it be called (`WriteLn`, report §9.2).
                if params.is_empty() && result.is_none() {
                    let _ = writeln!(s, "PROC {name}");
                } else {
                    let _ = write!(s, "PROC {name}({})", params.join("; "));
                    match result {
                        Some(r) => {
                            let _ = writeln!(s, ": {r}");
                        }
                        None => s.push('\n'),
                    }
                }
            }
        }
    }
    s
}

pub fn parse(text: &str) -> Result<Interface, String> {
    let mut it = Interface::default();
    let mut seen_module = false;
    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let lineno = lineno + 1;
        if let Some(rest) = line.strip_prefix("MODULE ") {
            if seen_module {
                return Err(format!("line {lineno}: duplicate MODULE line"));
            }
            if rest.split_whitespace().count() != 1 {
                return Err(format!("line {lineno}: malformed module name: {rest:?}"));
            }
            it.module = rest.to_string();
            seen_module = true;
            continue;
        }
        if !seen_module {
            return Err(format!("line {lineno}: declaration before MODULE line"));
        }
        if let Some(rest) = line.strip_prefix("IMPORT ") {
            for m in rest.split(',') {
                let m = m.trim();
                if m.is_empty() {
                    return Err(format!("line {lineno}: empty module name in IMPORT"));
                }
                it.imports.push(m.to_string());
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("CONST ") {
            let (name, value) = rest
                .split_once(" = ")
                .ok_or_else(|| format!("line {lineno}: CONST without ` = `"))?;
            it.items.push(Item::Const {
                name: name.to_string(),
                value: value.to_string(),
            });
        } else if let Some(rest) = line.strip_prefix("TYPE ") {
            let (name, def) = rest
                .split_once(" = ")
                .ok_or_else(|| format!("line {lineno}: TYPE without ` = `"))?;
            it.items.push(Item::Type {
                name: name.to_string(),
                def: def.to_string(),
            });
        } else if let Some(rest) = line.strip_prefix("VAR ") {
            let (name, ty) = rest
                .split_once(": ")
                .ok_or_else(|| format!("line {lineno}: VAR without `: `"))?;
            it.items.push(Item::Var {
                name: name.to_string(),
                ty: ty.to_string(),
            });
        } else if let Some(rest) = line.strip_prefix("PROC ") {
            match rest.split_once('(') {
                // `PROC WriteLn`: a parameterless procedure.
                None => {
                    if rest.contains(')') || rest.contains(':') {
                        return Err(format!("line {lineno}: malformed PROC: {rest:?}"));
                    }
                    let name = rest.trim();
                    if name.is_empty() || name.contains(' ') {
                        return Err(format!("line {lineno}: malformed PROC name: {rest:?}"));
                    }
                    it.items.push(Item::Proc {
                        name: name.to_string(),
                        params: Vec::new(),
                        result: None,
                    });
                }
                Some((name, tail)) => {
                    let (params, after) = tail
                        .split_once(')')
                        .ok_or_else(|| format!("line {lineno}: PROC without `)`"))?;
                    let params: Vec<String> = if params.is_empty() {
                        Vec::new()
                    } else {
                        params.split("; ").map(str::to_string).collect()
                    };
                    // A formal parameter's type is `{ARRAY OF} qualident`
                    // (report §10.1), so a parameter spec can never contain
                    // parentheses; rejecting them keeps the closing `)` of the
                    // parameter list unambiguous.
                    if params.iter().any(|p| p.contains('(') || p.contains(')')) {
                        return Err(format!(
                            "line {lineno}: formal parameter types are `{{ARRAY OF}} qualident` \
                             (report 10.1), so no parentheses are allowed: {params:?}"
                        ));
                    }
                    let after = after.trim();
                    let result = if after.is_empty() {
                        None
                    } else if let Some(r) = after.strip_prefix(": ") {
                        Some(r.to_string())
                    } else {
                        return Err(format!("line {lineno}: malformed PROC result: {after:?}"));
                    };
                    it.items.push(Item::Proc {
                        name: name.to_string(),
                        params,
                        result,
                    });
                }
            }
        } else {
            return Err(format!("line {lineno}: unknown declaration: {line:?}"));
        }
    }
    if !seen_module {
        return Err("no MODULE line".to_string());
    }
    Ok(it)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact example written out in DESIGN.md §7.
    const OUT_SYM: &str = "\
MODULE Out
IMPORT Texts, Oberon
PROC Write(ch: CHAR)
PROC WriteInt(x: INTEGER; n: INTEGER)
PROC WriteLn
VAR W: Texts.Writer
CONST N = 100
TYPE Node = RECORD key: INTEGER; left, right: Tree END
";

    #[test]
    fn parses_the_design_example() {
        let i = parse(OUT_SYM).expect("design example must parse");
        assert_eq!(i.module, "Out");
        assert_eq!(i.imports, vec!["Texts".to_string(), "Oberon".to_string()]);
        assert_eq!(i.items.len(), 6);
        assert_eq!(
            i.items[0],
            Item::Proc {
                name: "Write".into(),
                params: vec!["ch: CHAR".into()],
                result: None
            }
        );
        assert_eq!(
            i.items[1],
            Item::Proc {
                name: "WriteInt".into(),
                params: vec!["x: INTEGER".into(), "n: INTEGER".into()],
                result: None
            }
        );
        // A parameterless procedure is written with an empty parameter list and
        // must come back as zero parameters, not one empty one.
        assert_eq!(
            i.items[2],
            Item::Proc {
                name: "WriteLn".into(),
                params: vec![],
                result: None
            }
        );
        assert_eq!(
            i.items[3],
            Item::Var {
                name: "W".into(),
                ty: "Texts.Writer".into()
            }
        );
        assert_eq!(
            i.items[5],
            Item::Type {
                name: "Node".into(),
                def: "RECORD key: INTEGER; left, right: Tree END".into()
            }
        );
    }

    #[test]
    fn round_trips_every_item_kind() {
        let i = Interface {
            module: "M".into(),
            imports: vec!["Texts".into(), "Oberon".into()],
            items: vec![
                Item::Const {
                    name: "N".into(),
                    value: "2*N - 1".into(),
                },
                Item::Type {
                    name: "T".into(),
                    def: "POINTER TO R".into(),
                },
                Item::Var {
                    name: "v".into(),
                    ty: "ARRAY 32 OF CHAR".into(),
                },
                Item::Proc {
                    name: "f".into(),
                    params: vec![
                        "VAR y: REAL".into(),
                        "ARRAY OF CHAR".into(),
                        "p: MyProcType".into(),
                    ],
                    result: Some("INTEGER".into()),
                },
                Item::Proc {
                    name: "g".into(),
                    params: vec![],
                    result: None,
                },
            ],
        };
        let text = render(&i);
        let back = parse(&text).expect("rendered interface must parse");
        assert_eq!(i, back, "round trip must be exact\n{text}");
        assert_eq!(text, render(&back), "render must be a fixed point");
    }

    #[test]
    fn rejects_malformed_lines() {
        assert!(parse("CONST N = 1").unwrap_err().contains("before MODULE"));
        assert!(parse("MODULE A\nMODULE B").unwrap_err().contains("duplicate"));
        assert!(parse("MODULE A\nCONST N 1")
            .unwrap_err()
            .contains("CONST without"));
        assert!(parse("MODULE A\nPROC f(x: INTEGER")
            .unwrap_err()
            .contains("PROC without `)`"));
        // report 10.1: a formal type is `{ARRAY OF} qualident`, never an
        // inline `PROCEDURE (...)`; the format refuses to be ambiguous about
        // where the parameter list ends.
        assert!(parse("MODULE A\nPROC f(p: PROCEDURE (x: INTEGER))")
            .unwrap_err()
            .contains("no parentheses are allowed"));
        assert!(parse("MODULE A\nPROC WriteLn: INTEGER")
            .unwrap_err()
            .contains("malformed PROC"));
        assert!(parse("MODULE A\nVAR x").unwrap_err().contains("VAR without"));
        assert!(parse("MODULE A\nFOO x").unwrap_err().contains("unknown"));
    }
}
