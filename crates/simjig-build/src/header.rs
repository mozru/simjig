//! Port declarations in the Verilator-generated model header, and the cross-check
//! against the ports the IR expects.
//!
//! Verilator declares each top-level port of `V<prefix>.h` with a macro from
//! `verilated_types.h`: `VL_IN8(&name, msb, lsb)` (and `VL_IN16`, `VL_IN` for 17-32 bits,
//! `VL_IN64`, `VL_INW(&name, msb, lsb, words)` for 65+ bits), likewise `VL_OUT*` and
//! `VL_INOUT*`. Reading them back after every build catches drift between what the type
//! IR says and what the C++ model really contains (DECISION.md §2 #6) before any
//! generated code touches model memory.
//!
//! Not covered: ports Verilator declares without a macro, i.e. unpacked arrays
//! (`VlUnpacked<..> &name;`) and `real` ports (`double &name;`). The IR side must leave
//! those out of the expected list.

use std::fmt;
use std::path::Path;

use svtypes::ir::Storage;

use crate::Error;

/// Port direction as declared in the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PortDir {
    /// `VL_IN*`.
    In,
    /// `VL_OUT*`.
    Out,
    /// `VL_INOUT*`.
    InOut,
}

/// One port declaration from `V<prefix>.h`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HeaderPort {
    /// The C++ member name (Verilator-mangled, e.g. `esc__024id` for `\esc$id`).
    pub name: String,
    /// Direction.
    pub dir: PortDir,
    /// Most significant bit index as Verilator normalised it (always `>= lsb`).
    pub msb: u32,
    /// Least significant bit index.
    pub lsb: u32,
    /// C++ storage type: always a packed one ([`svtypes::width::packed_storage`] of the
    /// width), since ports without a macro are not listed.
    pub storage: Storage,
}

impl HeaderPort {
    /// Width in bits.
    pub fn width(&self) -> u32 {
        self.msb.abs_diff(self.lsb) + 1
    }
}

impl fmt::Display for HeaderPort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} {}[{}:{}] as {:?}",
            self.dir, self.name, self.msb, self.lsb, self.storage
        )
    }
}

/// Reads and parses the port declarations of a generated model header.
pub fn read_header_ports(path: &Path) -> Result<Vec<HeaderPort>, Error> {
    let text = std::fs::read_to_string(path).map_err(Error::io("reading", path))?;
    parse_header_ports(path, &text)
}

/// Parses the port declarations out of the text of a generated model header, in
/// declaration order. `path` is used only in error messages.
pub fn parse_header_ports(path: &Path, text: &str) -> Result<Vec<HeaderPort>, Error> {
    let mut ports = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if !(line.starts_with("VL_IN") || line.starts_with("VL_OUT")) {
            continue;
        }
        let port = parse_decl(line).map_err(|msg| Error::Parse {
            path: path.to_path_buf(),
            line: i + 1,
            msg: format!("{msg} in port declaration `{line}`"),
        })?;
        ports.push(port);
    }
    Ok(ports)
}

fn parse_decl(line: &str) -> Result<HeaderPort, String> {
    let (mac, rest) = line.split_once('(').ok_or("missing `(`")?;
    let args = rest
        .strip_suffix(");")
        .ok_or("expected the declaration to end with `);`")?;
    let (dir, suffix) = if let Some(s) = mac.strip_prefix("VL_INOUT") {
        (PortDir::InOut, s)
    } else if let Some(s) = mac.strip_prefix("VL_IN") {
        (PortDir::In, s)
    } else if let Some(s) = mac.strip_prefix("VL_OUT") {
        (PortDir::Out, s)
    } else {
        return Err(format!("unknown macro `{mac}`"));
    };
    let args: Vec<&str> = args.split(',').map(str::trim).collect();
    let name = args
        .first()
        .and_then(|a| a.strip_prefix('&'))
        .filter(|n| !n.is_empty())
        .ok_or("expected `&name` as the first argument")?;
    let num = |i: usize, what: &str| -> Result<u32, String> {
        let a = args.get(i).ok_or_else(|| format!("missing {what}"))?;
        a.parse()
            .map_err(|_not_a_number| format!("{what} `{a}` is not a number"))
    };
    let (storage, arity) = match suffix {
        "8" => (Storage::CData, 3),
        "16" => (Storage::SData, 3),
        "" => (Storage::IData, 3),
        "64" => (Storage::QData, 3),
        "W" => (
            Storage::VlWide {
                words: num(3, "word count")?,
            },
            4,
        ),
        other => return Err(format!("unknown storage suffix `{other}`")),
    };
    if args.len() != arity {
        return Err(format!("expected {arity} arguments, found {}", args.len()));
    }
    Ok(HeaderPort {
        name: name.to_owned(),
        dir,
        msb: num(1, "msb")?,
        lsb: num(2, "lsb")?,
        storage,
    })
}

/// One disagreement between the header and the expected ports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortProblem {
    /// Expected, but the header has no such port.
    Missing(HeaderPort),
    /// In the header, but not expected.
    Unexpected(HeaderPort),
    /// Present in both with a different direction, range or storage.
    Differs {
        /// What the IR expected.
        expected: HeaderPort,
        /// What the header declares.
        header: HeaderPort,
    },
}

impl fmt::Display for PortProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(p) => write!(f, "missing from header: expected {p}"),
            Self::Unexpected(p) => write!(f, "not expected: header declares {p}"),
            Self::Differs { expected, header } => {
                write!(
                    f,
                    "`{}`: expected {expected}, header declares {header}",
                    header.name
                )
            }
        }
    }
}

/// The header and the IR disagree; lists every problem, sorted by port name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct PortMismatch {
    /// Every disagreement found.
    pub problems: Vec<PortProblem>,
}

impl fmt::Display for PortMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "the Verilator model header disagrees with the type IR on {} port(s):",
            self.problems.len()
        )?;
        for p in &self.problems {
            writeln!(f, "  - {p}")?;
        }
        write!(
            f,
            "this is a simjig bug (IR and Verilator drifted apart); please report it with \
             the Verilator version and the port declarations above"
        )
    }
}

/// Compares the header's ports with the expected ones, ignoring declaration order.
pub fn check_ports(header: &[HeaderPort], expected: &[HeaderPort]) -> Result<(), PortMismatch> {
    let find = |ports: &[HeaderPort], name: &str| ports.iter().find(|p| p.name == name).cloned();
    let mut problems = Vec::new();
    for e in expected {
        match find(header, &e.name) {
            None => problems.push(PortProblem::Missing(e.clone())),
            Some(h) if h != *e => problems.push(PortProblem::Differs {
                expected: e.clone(),
                header: h,
            }),
            Some(_) => {}
        }
    }
    for h in header {
        if find(expected, &h.name).is_none() {
            problems.push(PortProblem::Unexpected(h.clone()));
        }
    }
    problems.sort_by(|a, b| problem_name(a).cmp(problem_name(b)));
    if problems.is_empty() {
        Ok(())
    } else {
        Err(PortMismatch { problems })
    }
}

fn problem_name(p: &PortProblem) -> &str {
    match p {
        PortProblem::Missing(p) | PortProblem::Unexpected(p) => &p.name,
        PortProblem::Differs { header, .. } => &header.name,
    }
}

#[cfg(test)]
mod tests {
    use svtypes::width::packed_storage;

    use super::*;

    fn port(name: &str, dir: PortDir, msb: u32, lsb: u32) -> HeaderPort {
        HeaderPort {
            name: name.to_owned(),
            dir,
            msb,
            lsb,
            storage: packed_storage(msb - lsb + 1).unwrap(),
        }
    }

    #[test]
    fn parses_every_macro_form() {
        // Lines as Verilator 5.052 writes them (see `tests/it` for a real header).
        let text = "    VL_IN8(&clk,0,0);\n    VL_OUT16(&count,11,0);\n    VL_IN(&i17,16,0);\n\
                    VL_INOUT64(&b,32,0);\n    VL_OUTW(&w,69,0,3);\n    VL_OUT8(&off,3,2);\n\
                    VlUnpacked<CData/*7:0*/, 4> &arr;\n    VL_UNCOPYABLE(Vx);\n";
        let ports = parse_header_ports(Path::new("V.h"), text).unwrap();
        let want = [
            ("clk", PortDir::In, 0, 0, Storage::CData),
            ("count", PortDir::Out, 11, 0, Storage::SData),
            ("i17", PortDir::In, 16, 0, Storage::IData),
            ("b", PortDir::InOut, 32, 0, Storage::QData),
            ("w", PortDir::Out, 69, 0, Storage::VlWide { words: 3 }),
            ("off", PortDir::Out, 3, 2, Storage::CData),
        ];
        let got: Vec<_> = ports
            .iter()
            .map(|p| (p.name.as_str(), p.dir, p.msb, p.lsb, p.storage.clone()))
            .collect();
        assert_eq!(got, want);
        assert_eq!(ports[5].width(), 2);
    }

    #[test]
    fn malformed_declarations_name_the_line() {
        let err = |text: &str| {
            parse_header_ports(Path::new("V.h"), text)
                .unwrap_err()
                .to_string()
        };
        assert_eq!(
            err("x\n  VL_IN8(&a,x,0);\n"),
            "V.h:2: msb `x` is not a number in port declaration `VL_IN8(&a,x,0);`"
        );
        assert_eq!(
            err("VL_INW(&a,99,0);"),
            "V.h:1: missing word count in port declaration `VL_INW(&a,99,0);`"
        );
        assert_eq!(
            err("VL_IN32(&a,31,0);"),
            "V.h:1: unknown storage suffix `32` in port declaration `VL_IN32(&a,31,0);`"
        );
        assert_eq!(
            err("VL_IN8(a,0,0);"),
            "V.h:1: expected `&name` as the first argument in port declaration `VL_IN8(a,0,0);`"
        );
    }

    #[test]
    fn check_ports_accepts_any_order() {
        let a = port("a", PortDir::In, 7, 0);
        let b = port("b", PortDir::Out, 0, 0);
        assert_eq!(check_ports(&[a.clone(), b.clone()], &[b, a]), Ok(()));
    }

    #[test]
    fn check_ports_reports_every_problem_precisely() {
        let header = [
            port("clk", PortDir::In, 0, 0),
            port("data", PortDir::In, 15, 0),
            port("extra", PortDir::Out, 3, 0),
        ];
        let expected = [
            port("clk", PortDir::In, 0, 0),
            port("data", PortDir::In, 7, 0),
            port("valid", PortDir::Out, 0, 0),
        ];
        let err = check_ports(&header, &expected).unwrap_err();
        assert_eq!(
            err.problems,
            [
                PortProblem::Differs {
                    expected: expected[1].clone(),
                    header: header[1].clone()
                },
                PortProblem::Unexpected(header[2].clone()),
                PortProblem::Missing(expected[2].clone()),
            ]
        );
        let text = err.to_string();
        assert!(
            text.contains(
                "`data`: expected In data[7:0] as CData, header declares In data[15:0] as SData"
            ),
            "{text}"
        );
        assert!(
            text.contains("missing from header: expected Out valid[0:0] as CData"),
            "{text}"
        );
        assert!(
            text.contains("not expected: header declares Out extra[3:0] as CData"),
            "{text}"
        );
    }
}
