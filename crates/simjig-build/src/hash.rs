//! Stable content hashing for cache keys and model prefixes.
//!
//! Invariant: a key depends only on the bytes fed in, never on the process, the platform's
//! `Hash` impls or the Rust release. That is why this is SHA-256 rather than
//! `std::hash::DefaultHasher`, whose algorithm is explicitly unspecified. Every field is
//! length-prefixed, so `("ab", "c")` and `("a", "bc")` hash differently. For the same
//! reason no key hashes `Debug` output: its format is not a stable interface.
//!
//! The model's IR hash ([`ir_hash`]) uses the same scheme.

use std::path::Path;

use sha2::{Digest, Sha256};
use svtypes::ir::Design;

use crate::Error;
use crate::codegen::{CodegenConfig, IfaceBinding, Probe, ProbeAccess, ProbeType};

/// Accumulates length-prefixed fields into a SHA-256 digest.
#[derive(Debug, Clone, Default)]
pub(crate) struct KeyHasher(Sha256);

impl KeyHasher {
    /// Starts a hash in a named domain, so keys of different kinds never collide.
    pub(crate) fn new(domain: &str) -> Self {
        let mut h = Self::default();
        h.field(domain.as_bytes());
        h
    }

    pub(crate) fn field(&mut self, bytes: &[u8]) -> &mut Self {
        self.0.update((bytes.len() as u64).to_le_bytes());
        self.0.update(bytes);
        self
    }

    pub(crate) fn str(&mut self, s: &str) -> &mut Self {
        self.field(s.as_bytes())
    }

    pub(crate) fn path(&mut self, p: &Path) -> &mut Self {
        self.field(p.as_os_str().as_encoded_bytes())
    }

    /// Hashes a file's path and its contents.
    pub(crate) fn file(&mut self, p: &Path) -> Result<&mut Self, Error> {
        let contents = std::fs::read(p).map_err(Error::io("reading", p))?;
        self.path(p);
        Ok(self.field(&contents))
    }

    /// The full digest as 64 lowercase hex digits.
    pub(crate) fn finish(self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let digest = self.0.finalize();
        let mut hex = String::with_capacity(64);
        for byte in digest {
            hex.push(char::from(HEX[usize::from(byte >> 4)]));
            hex.push(char::from(HEX[usize::from(byte & 0xf)]));
        }
        hex
    }
}

/// The IR hash that the generated Rust and the C++ shim both carry, so a model built from
/// another IR is rejected at load: the first 64 bits of a [`KeyHasher`] digest of the IR
/// (as JSON, the IR's versioned form) and of every field of `config` that shapes the
/// generated code.
pub(crate) fn ir_hash(design: &Design, config: &CodegenConfig) -> u64 {
    let mut h = KeyHasher::new("simjig-ir-hash-v1");
    // Serialising plain derived structs to a Vec cannot fail.
    h.field(&serde_json::to_vec(design).unwrap_or_default());
    h.field(&(config.probes.len() as u64).to_le_bytes());
    for p in &config.probes {
        probe(&mut h, p);
    }
    match &config.iface_top {
        None => h.str("no-iface-top"),
        Some(top) => {
            h.str("iface-top").str(&top.dut);
            pairs(&mut h, &top.params);
            h.field(&(top.bindings.len() as u64).to_le_bytes());
            for b in &top.bindings {
                binding(&mut h, b);
            }
            &mut h
        }
    };
    let digest = h.0.finalize();
    let mut first = [0u8; 8];
    first.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(first)
}

fn probe(h: &mut KeyHasher, p: &Probe) {
    h.str(&p.path).str(&p.module);
    match &p.ty {
        ProbeType::Named(name) => h.str("named").str(name),
        ProbeType::Bits { width } => h.str("bits").field(&width.to_le_bytes()),
    };
    h.str(match p.access {
        ProbeAccess::Read => "read",
        ProbeAccess::ReadWrite => "read-write",
    });
}

fn binding(h: &mut KeyHasher, b: &IfaceBinding) {
    h.str(&b.port).str(&b.interface);
    pairs(h, &b.params);
    match &b.modport {
        None => h.str("no-modport"),
        Some(m) => h.str("modport").str(m),
    };
}

fn pairs(h: &mut KeyHasher, pairs: &[(String, String)]) {
    h.field(&(pairs.len() as u64).to_le_bytes());
    for (k, v) in pairs {
        h.str(k).str(v);
    }
}

#[cfg(test)]
mod tests {
    use svtypes::ir::{Module, ModuleId, ModuleKind, Producer, TimeExp};

    use super::*;
    use crate::codegen::{IfaceBinding, IfaceTop, Probe, ProbeAccess, ProbeType};

    fn design(top: &str) -> Design {
        Design {
            schema: svtypes::ir::SCHEMA,
            producer: Producer {
                tool: "verilator".into(),
                version: "5.052".into(),
            },
            time_precision: TimeExp(-12),
            top: ModuleId(0),
            types: Vec::new(),
            modules: vec![Module {
                name: top.into(),
                orig_name: top.into(),
                kind: ModuleKind::Module,
                params: Vec::new(),
                unrepresentable: Vec::new(),
                ports: Vec::new(),
                iface_ports: Vec::new(),
                modports: Vec::new(),
            }],
            packages: Vec::new(),
        }
    }

    fn probe(access: ProbeAccess) -> Probe {
        Probe {
            path: "t.u.s".into(),
            module: "u".into(),
            ty: ProbeType::Bits { width: 4 },
            access,
        }
    }

    #[test]
    fn ir_hash_is_pinned() {
        // Pinned: the hash is compiled into generated Rust and C++ and compared at load,
        // so it must not depend on the Rust release (as `Debug` output may). Changing it
        // needs a CHANGELOG entry. Reference value from Python: sha256 over (u64 LE length,
        // bytes) of the domain, the IR JSON (schema 2), a zero probe count and
        // "no-iface-top"; the first 8 bytes, big-endian.
        assert_eq!(
            ir_hash(&design("t"), &CodegenConfig::default()),
            0x7602_a740_1ba1_fbe5
        );
    }

    #[test]
    fn ir_hash_tracks_the_design_and_every_config_field() {
        let base = CodegenConfig {
            probes: vec![probe(ProbeAccess::Read)],
            ..CodegenConfig::default()
        };
        let binding = IfaceBinding {
            port: "s".into(),
            interface: "i".into(),
            params: Vec::new(),
            modport: None,
        };
        let iface = |b: IfaceBinding| {
            Some(IfaceTop {
                dut: "t".into(),
                params: Vec::new(),
                bindings: vec![b],
            })
        };
        let variants = [
            CodegenConfig {
                probes: vec![probe(ProbeAccess::ReadWrite)],
                ..base.clone()
            },
            CodegenConfig {
                probes: vec![Probe {
                    ty: ProbeType::Named("pkg::t".into()),
                    ..probe(ProbeAccess::Read)
                }],
                ..base.clone()
            },
            CodegenConfig {
                probes: Vec::new(),
                ..base.clone()
            },
            CodegenConfig {
                iface_top: iface(binding.clone()),
                ..base.clone()
            },
            CodegenConfig {
                iface_top: iface(IfaceBinding {
                    modport: Some("m".into()),
                    ..binding.clone()
                }),
                ..base.clone()
            },
            CodegenConfig {
                iface_top: iface(IfaceBinding {
                    params: vec![("W".into(), "8".into())],
                    ..binding
                }),
                ..base.clone()
            },
        ];
        let mut seen = vec![ir_hash(&design("t"), &base), ir_hash(&design("u"), &base)];
        for v in &variants {
            seen.push(ir_hash(&design("t"), v));
        }
        let mut unique = seen.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), seen.len(), "{seen:x?}");
        assert_eq!(
            ir_hash(&design("t"), &base),
            ir_hash(&design("t"), &base.clone())
        );
    }

    #[test]
    fn known_digest_is_stable() {
        // Pinned value: if this changes, every user's cache is invalidated and every model
        // prefix changes. Only change it together with a CHANGELOG entry. Reference value from
        // Python: sha256 over (u64 LE length, bytes) for "test" then "hello".
        let mut h = KeyHasher::new("test");
        h.str("hello");
        assert_eq!(
            h.finish(),
            "b2a1b72f2e76e6de7a45696f907cdf3732933dfab9843b886202630d4405a0b8"
        );
    }

    #[test]
    fn fields_are_length_prefixed() {
        let mut a = KeyHasher::new("d");
        a.str("ab").str("c");
        let mut b = KeyHasher::new("d");
        b.str("a").str("bc");
        assert_ne!(a.finish(), b.finish());
    }

    #[test]
    fn domains_separate_keys() {
        let mut a = KeyHasher::new("model");
        a.str("x");
        let mut b = KeyHasher::new("lib");
        b.str("x");
        assert_ne!(a.finish(), b.finish());
    }

    #[test]
    fn file_hash_tracks_contents() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.sv");
        std::fs::write(&p, "module a; endmodule\n").unwrap();
        let hash = || {
            let mut h = KeyHasher::new("f");
            h.file(&p).unwrap();
            h.finish()
        };
        let first = hash();
        assert_eq!(first, hash(), "same contents, same key");
        std::fs::write(&p, "module a; wire w; endmodule\n").unwrap();
        assert_ne!(first, hash(), "edited contents, new key");
    }
}
