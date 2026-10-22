fn main() {
    // libduckdb.dylib ships in Xenon.app/Contents/Frameworks; next to a bare
    // `target/<profile>/xenon` it's in `deps/`, where the duckdb crate copies it.
    // The app bundle (XENON_BUNDLE, set by the release scripts) only looks in Frameworks.
    println!("cargo:rerun-if-env-changed=XENON_BUNDLE");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg-bins=-Wl,-rpath,@executable_path/../Frameworks");
        if std::env::var_os("XENON_BUNDLE").is_none() {
            println!("cargo:rustc-link-arg-bins=-Wl,-rpath,@executable_path/deps");
        }
    }
}
