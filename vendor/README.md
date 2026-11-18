# Vendored crates

Only crates that this repository has to build against a target their release
does not compile for live here. Each directory is the exact upstream release
named in its `Cargo.toml`, with the change described below and no other edit.
`rust/Cargo.toml` selects the copies with `[patch.crates-io]`.

## truce-rack-clap 1.1.5 and truce-rack-vst3 1.1.5

Both spell VST3 and CLAP `char` buffers as `i8`. `c_char` is `u8` on AArch64
Linux, so the portable arm64 AppImage does not compile against the releases on
crates.io. The copies use `c_char`, which is the type both the generated
bindings and the C ABI actually use on every target.

Delete the patches, these directories, and their lock entries once an upstream
release carries the fix; 1.1.5 is the newest on crates.io as of this change.
Both crates are MIT OR Apache-2.0, and `third-party-licenses.md` lists them.