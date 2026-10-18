//! Cross-checks `Port::storage` and `Port::cpp_member` against the model header that
//! `verilator --cc` generates for every fixture.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use svtypes::ir::Storage;

use crate::Case;

#[test]
fn storage_matches_generated_headers() {
    for case in Case::all() {
        let design = case.committed().unwrap();
        let header = generate_header(&case);
        let actual = parse_header(&header);
        let expected: BTreeMap<String, (Storage, Option<u32>)> = design
            .top_module()
            .ports
            .iter()
            .map(|p| {
                let member = p.cpp_member.clone().unwrap();
                (member, (p.storage.clone(), design.ty(p.ty).width))
            })
            .collect();
        assert_eq!(
            actual, expected,
            "{}: header and IR disagree\n{header}",
            case.name
        );
    }
}

fn generate_header(case: &Case) -> String {
    let opts = case.opts();
    let out = scratch_dir(&case.name);
    let mut cmd = Command::new("verilator");
    cmd.args([
        "--cc",
        "--no-timing",
        "-Wno-fatal",
        "--top-module",
        &opts.top,
        "--prefix",
        "Vfixture",
    ]);
    cmd.arg("-Mdir").arg(&out);
    for (name, value) in &opts.params {
        cmd.arg(format!("-G{name}={value}"));
    }
    cmd.args(&opts.sources);
    let status = cmd.status().unwrap();
    assert!(status.success(), "{}: verilator --cc failed", case.name);
    let header = fs::read_to_string(out.join("Vfixture.h")).unwrap();
    fs::remove_dir_all(&out).unwrap();
    header
}

fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("svtypes-header-{}-{name}", std::process::id()));
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Port members of the model class: `VL_IN8(&a,7,0);` and `VlUnpacked<CData/*7:0*/, 4> &ua;`.
/// The width is `msb - lsb + 1` for packed ports and `None` for unpacked ones.
fn parse_header(header: &str) -> BTreeMap<String, (Storage, Option<u32>)> {
    let mut ports = BTreeMap::new();
    for line in header.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("VlUnpacked<") {
            let decl = strip_comments(&format!("VlUnpacked<{rest}"));
            let (ty, name) = decl.rsplit_once(" &").unwrap();
            ports.insert(
                name.trim_end_matches(';').to_owned(),
                (parse_storage(ty), None),
            );
            continue;
        }
        let Some((mac, args)) = line.split_once('(') else {
            continue;
        };
        let suffix = ["VL_INOUT", "VL_IN", "VL_OUT"]
            .iter()
            .find_map(|p| mac.strip_prefix(p));
        let Some(suffix) = suffix else { continue };
        let args: Vec<&str> = args.trim_end_matches(");").split(',').collect();
        let msb: i64 = args[1].parse().unwrap();
        let lsb: i64 = args[2].parse().unwrap();
        let storage = match suffix {
            "8" => Storage::CData,
            "16" => Storage::SData,
            "" => Storage::IData,
            "64" => Storage::QData,
            "W" => Storage::VlWide {
                words: args[3].parse().unwrap(),
            },
            other => panic!("unknown port macro suffix {other} in {line}"),
        };
        let width = u32::try_from(msb - lsb + 1).unwrap();
        ports.insert(
            args[0].trim_start_matches('&').to_owned(),
            (storage, Some(width)),
        );
    }
    ports
}

fn strip_comments(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some((before, after)) = rest.split_once("/*") {
        out.push_str(before);
        rest = after.split_once("*/").map_or("", |(_, tail)| tail);
    }
    out.push_str(rest);
    out
}

fn parse_storage(text: &str) -> Storage {
    let text = text.trim();
    match text {
        "CData" => Storage::CData,
        "SData" => Storage::SData,
        "IData" => Storage::IData,
        "QData" => Storage::QData,
        _ => {
            if let Some(n) = text
                .strip_prefix("VlWide<")
                .and_then(|t| t.strip_suffix('>'))
            {
                return Storage::VlWide {
                    words: n.trim().parse().unwrap(),
                };
            }
            let inner = text
                .strip_prefix("VlUnpacked<")
                .and_then(|t| t.strip_suffix('>'))
                .unwrap();
            let (elem, len) = inner.rsplit_once(',').unwrap();
            Storage::VlUnpacked {
                elem: Box::new(parse_storage(elem)),
                len: len.trim().parse().unwrap(),
            }
        }
    }
}

#[test]
fn header_parser_reads_verilator_declarations() {
    let header = "    VL_IN8(&a,7,0);\n    VL_INW(&w,96,0,4);\n    VL_OUT64(&q,63,0);\n    VL_IN8(&neg,3,-2);\n    VlUnpacked<VlUnpacked<CData/*7:0*/, 2>, 4> &dyn;\n";
    let ports = parse_header(header);
    assert_eq!(ports["a"], (Storage::CData, Some(8)));
    assert_eq!(ports["w"], (Storage::VlWide { words: 4 }, Some(97)));
    assert_eq!(ports["q"], (Storage::QData, Some(64)));
    assert_eq!(ports["neg"], (Storage::CData, Some(6)));
    let inner = Storage::VlUnpacked {
        elem: Box::new(Storage::CData),
        len: 2,
    };
    assert_eq!(
        ports["dyn"],
        (
            Storage::VlUnpacked {
                elem: Box::new(inner),
                len: 4
            },
            None
        )
    );
}
