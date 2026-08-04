//! Target-specific linker configuration for the `csgraph` executable.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        // MSVC executables default to a 1 MiB main stack, which is insufficient for the
        // synchronous extraction worker entered from the async CLI dispatcher.
        println!("cargo:rustc-link-arg-bin=csgraph=/STACK:8388608");
    }
}
