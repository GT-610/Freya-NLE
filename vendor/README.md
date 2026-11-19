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

Both crates are MIT OR Apache-2.0; each directory carries the upstream
`LICENSE-MIT` and `LICENSE-APACHE`, and the same texts ship in the artifacts
as `licenses/MIT.txt` and `licenses/Apache-2.0.txt`.

Dropping the patches is a checkout that has to end with a compiling arm64
build, not just deleted files:

1. Check whether an upstream release carries the fix (1.1.5 was the newest on
   crates.io as of this change). Bump `truce-rack` in `freya-nle-audio` to it,
   or `cargo update -p truce-rack-clap -p truce-rack-vst3`, so `Cargo.lock`
   selects registry sources again.
2. Build the arm64 AppImage with `--locked` to confirm the registry versions
   really fix it.
3. Remove the `[patch.crates-io]` entries and these directories, and commit
   the regenerated `Cargo.lock` together with them.