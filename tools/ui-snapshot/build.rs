fn main() {
    // No bundled catalogs here: the preview shows the source (English) strings.
    // The editor's own build.rs loads the .po files for the shipping UI.
    let config = slint_build::CompilerConfiguration::new()
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    slint_build::compile_with_config("../../rust/freya-nle/ui/app.slint", config)
        .expect("compile Slint UI");
}
