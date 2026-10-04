//! Compile the cxx bridge. When `TERN_FFI_INCLUDE_DIR` is set (by CMake), the
//! generated headers (`tern-ffi/src/lib.rs.h`, `rust/cxx.h`) are copied there
//! so the Qt frontend can include them from a stable path.

use std::path::{Path, PathBuf};

fn copy_tree(from: &Path, to: &Path) {
    let Ok(entries) = std::fs::read_dir(from) else { return };
    for e in entries.flatten() {
        let src = e.path();
        let dst = to.join(e.file_name());
        // `metadata` follows symlinks (cxx-build links some headers).
        let Ok(meta) = std::fs::metadata(&src) else { continue };
        if meta.is_dir() {
            std::fs::create_dir_all(&dst).expect("create include dir");
            copy_tree(&src, &dst);
        } else if src.extension().is_some_and(|x| x == "h") {
            let new = std::fs::read(&src).expect("read generated header");
            // Don't touch unchanged headers: that would rebuild all C++.
            if std::fs::read(&dst).ok().as_deref() != Some(&new[..]) {
                std::fs::write(&dst, new).expect("write generated header");
            }
        }
    }
}

fn main() {
    cxx_build::bridge("src/lib.rs").std("c++20").compile("tern-ffi-bridge");
    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=include/sink.h");
    println!("cargo:rerun-if-env-changed=TERN_FFI_INCLUDE_DIR");

    if let Some(dir) = std::env::var_os("TERN_FFI_INCLUDE_DIR") {
        let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).expect("create TERN_FFI_INCLUDE_DIR");
        copy_tree(&out.join("cxxbridge/include"), &dir);
    }
}
