fn main() {
    // libduckdb.dylib ships in Xenon.app/Contents/Frameworks; next to a bare
    // `target/<profile>/xenon` it's in `deps/`, where the duckdb crate copies it.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg-bins=-Wl,-rpath,@executable_path/../Frameworks");
        println!("cargo:rustc-link-arg-bins=-Wl,-rpath,@executable_path/deps");
    }
}
