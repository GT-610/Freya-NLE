//! Shader probes that measured what a GPU-less Windows host can compile.
//!
//! Each mode is the entry point of one historical probe; they share the
//! compositor shader assembly in `shader.rs` and nothing else, because the
//! questions they asked (size, capability, sample rewriting) differ.

mod app;
mod cap;
mod fragment;
mod pipeline;
mod rewrite;
mod shader;
mod size;

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    match mode.as_str() {
        "app" => app::run(),
        "size" => size::run(),
        "pipeline" => pipeline::run(),
        "cap" => cap::run(),
        "fragment" => fragment::run(),
        "rewrite" => rewrite::run(),
        other => {
            eprintln!(
                "usage: shader-probe <app|cap|fragment|pipeline|rewrite|size> (got {other:?})"
            );
            std::process::exit(2);
        }
    }
}
