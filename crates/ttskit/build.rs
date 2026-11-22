//! Gives the `ttskit` executable an 8 MiB main-thread stack on Windows.
//!
//! The default there is 1 MiB, and in a debug build the derived argument
//! parser of all subcommands needs more than that.

fn main() {
    const STACK_BYTES: u32 = 8 * 1024 * 1024;
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os == "windows" {
        if env == "msvc" {
            println!("cargo:rustc-link-arg-bins=/STACK:{STACK_BYTES}");
        } else if env == "gnu" {
            println!("cargo:rustc-link-arg-bins=-Wl,--stack,{STACK_BYTES}");
        }
    }
}
