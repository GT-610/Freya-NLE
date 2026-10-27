use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Locales that translate the Japanese effect and object metadata. Japanese is
/// the source language, so it needs no catalog of its own.
const EFFECT_METADATA_LOCALES: &[(&str, &str)] = &[
    ("en_US", "EFFECT_METADATA_ENGLISH"),
    ("zh_CN", "EFFECT_METADATA_SIMPLIFIED_CHINESE"),
];

fn main() {
    println!("cargo:rerun-if-changed=../../i18n/effect-metadata.json");
    generate_effect_metadata_translations().expect("generate effect metadata translations");

    let config = slint_build::CompilerConfiguration::new()
        .with_bundled_translations("translations")
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    slint_build::compile_with_config("ui/app.slint", config).expect("compile Slint UI");
}

fn generate_effect_metadata_translations() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let catalog_path = manifest_dir.join("../../i18n/effect-metadata.json");
    let output = PathBuf::from(env::var("OUT_DIR")?).join("effect_metadata_translations.rs");
    let mut generated = String::new();
    for (locale, symbol) in EFFECT_METADATA_LOCALES {
        let translations = read_effect_metadata_translations(&catalog_path, locale)?;
        write_translation_array(&mut generated, symbol, &translations);
    }
    fs::write(output, generated)?;
    Ok(())
}

fn read_effect_metadata_translations(
    path: &Path,
    locale: &str,
) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    let catalog: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
    let entries = catalog
        .get(locale)
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| format!("{} has no {:?} translations", path.display(), locale))?;
    let mut translations = entries
        .iter()
        .filter_map(|(source, translation)| {
            let translation = translation.as_str()?;
            (!source.is_empty() && !translation.is_empty())
                .then(|| (source.clone(), translation.to_owned()))
        })
        .collect::<Vec<_>>();
    // `localized_effect_metadata` binary-searches the generated table.
    translations.sort_by(|left, right| left.0.cmp(&right.0));
    translations.dedup_by(|left, right| left.0 == right.0);
    Ok(translations)
}

fn write_translation_array(output: &mut String, name: &str, translations: &[(String, String)]) {
    output.push_str(&format!("const {name}: &[(&str, &str)] = &[\n"));
    for (source, translation) in translations {
        output.push_str(&format!("    ({source:?}, {translation:?}),\n"));
    }
    output.push_str("];\n");
}
