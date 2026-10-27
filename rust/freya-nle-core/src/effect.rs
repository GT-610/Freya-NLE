use serde_json::{Map, Value};

fn string(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str)
}

fn valid_version(version: &str) -> bool {
    let mut parts = version.split('.');
    let valid_part =
        |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    let valid = parts.by_ref().take(3).all(valid_part);
    valid && parts.next().is_none() && version.split('.').count() == 3
}

fn valid_runtime_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && path.split('/').all(|component| {
            !component.is_empty()
                && component != "."
                && component != ".."
                && !component.contains(':')
        })
}

fn valid_uniform_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

fn normalize_runtime(value: &Value) -> Option<Map<String, Value>> {
    let runtime = value.as_object()?;
    if string(runtime.get("engine")) != Some("aviqtl-wgsl-v1") {
        return None;
    }
    let shader = string(runtime.get("shader"))?;
    if !valid_runtime_path(shader) || !shader.ends_with(".wgsl") {
        return None;
    }
    let uniforms = runtime
        .get("uniforms")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(Value::as_str)
        .collect::<Option<Vec<_>>>()?;
    if uniforms.len() > 16
        || uniforms
            .iter()
            .any(|uniform| uniform.len() > 64 || !valid_uniform_name(uniform))
        || uniforms
            .iter()
            .enumerate()
            .any(|(index, uniform)| uniforms[..index].contains(uniform))
    {
        return None;
    }
    Some(Map::from_iter([
        (
            "engine".to_owned(),
            Value::String("aviqtl-wgsl-v1".to_owned()),
        ),
        ("shader".to_owned(), Value::String(shader.to_owned())),
        (
            "uniforms".to_owned(),
            Value::Array(
                uniforms
                    .into_iter()
                    .map(|uniform| Value::String(uniform.to_owned()))
                    .collect(),
            ),
        ),
    ]))
}

pub(crate) fn normalize_metadata(input: &[u8]) -> Option<Map<String, Value>> {
    let mut metadata = serde_json::from_slice::<Value>(input)
        .ok()?
        .as_object()
        .cloned()?;
    let id = string(metadata.get("id"))?;
    let name = string(metadata.get("name"))?;
    // A built-in definition is drawn by the compiled-in renderer and needs no
    // shader of its own. Everything else must declare a native WGSL runtime;
    // metadata that only names a QML component is a legacy Qt definition and is
    // no longer a usable entry.
    let builtin = metadata
        .get("builtin")
        .and_then(Value::as_bool)
        .is_some_and(|builtin| builtin);
    let runtime = metadata.get("runtime").and_then(normalize_runtime);
    if id.is_empty() || name.is_empty() || (!builtin && runtime.is_none()) {
        return None;
    }
    let version = string(metadata.get("version"))?;
    if !valid_version(version) {
        return None;
    }
    if !matches!(string(metadata.get("kind")), Some("effect" | "object")) {
        return None;
    }
    let categories: Vec<Value> = metadata
        .get("categories")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(|category| Value::String(category.to_owned()))
        .collect();
    if categories.is_empty() {
        return None;
    }
    let ui = metadata.get("ui")?.as_object()?.clone();
    if !ui.contains_key("controls") {
        return None;
    }
    let params = metadata
        .get("params")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    metadata.insert("categories".to_owned(), Value::Array(categories));
    metadata.insert("params".to_owned(), Value::Object(params));
    metadata.insert("ui".to_owned(), Value::Object(ui));
    if let Some(runtime) = runtime {
        metadata.insert("runtime".to_owned(), Value::Object(runtime));
    }
    Some(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn metadata_schema_accepts_effects_and_normalizes_objects() {
        let input = json!({
            "id": "effect.test",
            "name": "Test",
            "builtin": true,
            "version": "1.2.3",
            "kind": "effect",
            "categories": ["Color", 5],
            "params": {"amount": 1},
            "ui": {"controls": []}
        });
        let normalized =
            normalize_metadata(&serde_json::to_vec(&input).expect("serialize metadata fixture"))
                .expect("valid metadata");
        assert_eq!(normalized.get("categories"), Some(&json!(["Color"])));
        assert_eq!(normalized.get("params"), Some(&json!({"amount": 1})));
    }

    #[test]
    fn metadata_schema_rejects_invalid_versions_kinds_and_controls() {
        for invalid in [
            json!({"id":"x","name":"x","builtin":true,"version":"1.0","kind":"effect","categories":["x"],"ui":{"controls":[]}}),
            json!({"id":"x","name":"x","builtin":true,"version":"1.0.0","kind":"filter","categories":["x"],"ui":{"controls":[]}}),
            json!({"id":"x","name":"x","builtin":true,"version":"1.0.0","kind":"effect","categories":["x"],"ui":{}}),
            json!({"id":"x","name":"x","version":"1.0.0","kind":"effect","categories":["x"],"ui":{"controls":[]}}),
            json!({"id":"x","name":"x","qml":"Legacy.qml","version":"1.0.0","kind":"effect","categories":["x"],"ui":{"controls":[]}}),
        ] {
            assert!(
                normalize_metadata(
                    &serde_json::to_vec(&invalid).expect("serialize metadata fixture")
                )
                .is_none()
            );
        }
    }

    #[test]
    fn metadata_schema_accepts_native_wgsl_without_a_builtin_marker() {
        let input = json!({
            "id": "effect.native",
            "name": "Native effect",
            "version": "1.0.0",
            "kind": "effect",
            "categories": ["Native"],
            "params": {"amount": 0.5},
            "ui": {"controls": []},
            "runtime": {
                "engine": "aviqtl-wgsl-v1",
                "shader": "shaders/main.wgsl",
                "uniforms": ["amount"]
            }
        });
        let normalized =
            normalize_metadata(&serde_json::to_vec(&input).expect("serialize metadata fixture"))
                .expect("valid native metadata");
        assert_eq!(normalized["runtime"]["engine"], "aviqtl-wgsl-v1");
        assert_eq!(normalized["runtime"]["shader"], "shaders/main.wgsl");
        assert_eq!(normalized["runtime"]["uniforms"], json!(["amount"]));
    }

    #[test]
    fn metadata_schema_rejects_invalid_native_runtimes() {
        let native = |runtime: Value| {
            json!({
                "id": "effect.native",
                "name": "Native effect",
                "version": "1.0.0",
                "kind": "effect",
                "categories": ["Native"],
                "params": {},
                "ui": {"controls": []},
                "runtime": runtime
            })
        };
        for invalid in [
            native(json!({"engine":"other","shader":"main.wgsl","uniforms":[]})),
            native(json!({"engine":"aviqtl-wgsl-v1","shader":"../main.wgsl","uniforms":[]})),
            native(json!({"engine":"aviqtl-wgsl-v1","shader":"C:/main.wgsl","uniforms":[]})),
            native(json!({"engine":"aviqtl-wgsl-v1","shader":"main.txt","uniforms":[]})),
            native(json!({"engine":"aviqtl-wgsl-v1","shader":"main.wgsl","uniforms":["bad-name"]})),
            native(
                json!({"engine":"aviqtl-wgsl-v1","shader":"main.wgsl","uniforms":["amount", "amount"]}),
            ),
            native(json!({
                "engine":"aviqtl-wgsl-v1",
                "shader":"main.wgsl",
                "uniforms":["a","b","c","d","e","f","g","h","i","j","k","l","m","n","o","p","q"]
            })),
        ] {
            assert!(
                normalize_metadata(
                    &serde_json::to_vec(&invalid).expect("serialize metadata fixture")
                )
                .is_none()
            );
        }
    }
}
