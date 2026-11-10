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
    embed_windows_resources();

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

/// Attach the application icon and a VERSIONINFO block to the Windows binary.
///
/// Without a resource section Explorer shows `freya-nle.exe` with a generic icon
/// and no version, which makes a redistributed copy impossible to identify.
/// The icon comes from `assets/icon.ico`; `tools/gen_app_icon.py` regenerates it
/// from `assets/icon.svg`, so the SVG stays the single source of the artwork.
#[cfg(windows)]
fn embed_windows_resources() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let icon = manifest_dir.join("../../assets/icon.ico");
    if !icon.is_file() {
        println!("cargo:warning=assets/icon.ico is missing; the executable keeps the default icon");
        return;
    }
    println!("cargo:rerun-if-changed={}", icon.display());

    let mut resource = winresource::WindowsResource::new();
    resource.set_icon(icon.to_str().expect("icon path is valid UTF-8"));
    resource.set("FileDescription", "Freya NLE");
    resource.set("ProductName", "Freya NLE");
    resource.set("LegalCopyright", "Copyright (C) Freya NLE contributors");
    // The workspace version is the single source of the project version, so it
    // is read from the crate version cargo already resolved rather than parsed
    // out of a second file.
    if let Ok(version) = env::var("CARGO_PKG_VERSION") {
        let numeric = version
            .split(['-', '+'])
            .next()
            .unwrap_or("0.0.0")
            .to_owned();
        // A resource version is four independent integers, so a prerelease
        // suffix cannot be carried and is recorded as the trailing field.
        let parts: Vec<u64> = numeric
            .split('.')
            .filter_map(|part| part.parse().ok())
            .collect();
        let mut fields = [0u64; 4];
        for (slot, value) in fields.iter_mut().zip(parts) {
            *slot = value;
        }
        resource.set_version_info(
            winresource::VersionInfo::default()
                .set_file_version(
                    fields[0] as u16,
                    fields[1] as u16,
                    fields[2] as u16,
                    fields[3] as u16,
                )
                .set_product_version(
                    fields[0] as u16,
                    fields[1] as u16,
                    fields[2] as u16,
                    fields[3] as u16,
                ),
        );
        resource.set("FileVersion", &version);
        resource.set("ProductVersion", &version);
        println!("cargo:rustc-env=FREYA_NLE_DISPLAY_VERSION={version}");
    }
    if let Err(error) = resource.compile() {
        // A missing resource compiler must not turn a working Windows build into
        // a failure; the application is usable without an icon.
        println!("cargo:warning=could not embed Windows resources: {error}");
    }
}

#[cfg(not(windows))]
fn embed_windows_resources() {}
