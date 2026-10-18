//! The hand-written C ABI runtime (`crates/simjig-build/cpp/`), embedded in this crate.
//!
//! The files are compiled into the library with `include_str!` rather than found through
//! `CARGO_MANIFEST_DIR` at run time, because `simjig-build` runs inside *other* crates'
//! build scripts: a vendored, registry or relocated source tree must still find them.
//! [`Runtime::extract`] writes them into the shared cache under a directory named by the
//! hash of their contents, so the path changes exactly when a file does. That path is
//! part of the libverilated cache key (via the `-include`/`-I` flags), which is how the
//! archive is keyed on the `simjig_vl_config.h` hash (docs/notes/abi.md, step 3).

use std::path::{Path, PathBuf};

use crate::Error;
use crate::cache::Cache;

/// Every file of `cpp/` except the spike, as (name, contents).
pub(crate) const FILES: &[(&str, &str)] = &[
    ("simjig_abi.h", include_str!("../cpp/simjig_abi.h")),
    ("simjig_model.h", include_str!("../cpp/simjig_model.h")),
    ("simjig_rt.cpp", include_str!("../cpp/simjig_rt.cpp")),
    ("simjig_rt.h", include_str!("../cpp/simjig_rt.h")),
    (
        "simjig_vl_config.h",
        include_str!("../cpp/simjig_vl_config.h"),
    ),
];

/// The runtime sources, written out to a content-addressed directory.
#[derive(Debug, Clone)]
pub(crate) struct Runtime {
    dir: PathBuf,
}

impl Runtime {
    /// Writes the embedded files into the cache (once per content) and returns them.
    pub(crate) fn extract(cache: &Cache) -> Result<Self, Error> {
        Ok(Self {
            dir: cache.store_files("runtime", FILES)?,
        })
    }

    /// The include directory holding `simjig_abi.h`, `simjig_model.h` and `simjig_rt.h`.
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// `simjig_vl_config.h`, force-included into every translation unit.
    pub(crate) fn config_header(&self) -> PathBuf {
        self.dir.join("simjig_vl_config.h")
    }

    /// `simjig_rt.cpp`, compiled into the shared libverilated archive.
    pub(crate) fn source(&self) -> PathBuf {
        self.dir.join("simjig_rt.cpp")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file added to `cpp/` but not to [`FILES`] would be missing in dependent crates.
    #[test]
    fn every_cpp_file_is_embedded() {
        let cpp = Path::new(env!("CARGO_MANIFEST_DIR")).join("cpp");
        let mut on_disk: Vec<String> = std::fs::read_dir(&cpp)
            .unwrap()
            .map(|e| e.unwrap())
            .filter(|e| e.file_type().unwrap().is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        on_disk.sort();
        let embedded: Vec<&str> = FILES.iter().map(|(n, _)| *n).collect();
        assert_eq!(on_disk, embedded);
    }

    #[test]
    fn extraction_is_content_addressed() {
        let t = tempfile::tempdir().unwrap();
        let cache = Cache::open(t.path().to_path_buf()).unwrap();
        let a = Runtime::extract(&cache).unwrap();
        assert_eq!(a.dir(), Runtime::extract(&cache).unwrap().dir());
        assert!(a.dir().starts_with(t.path().join("runtime")));
        let header = std::fs::read_to_string(a.config_header()).unwrap();
        assert!(header.contains("#define VL_TIME_CONTEXT"), "{header}");
        assert!(a.source().is_file());
    }
}
