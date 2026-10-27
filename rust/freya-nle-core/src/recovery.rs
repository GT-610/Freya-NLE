use crate::policy::{valid_recovery_id, valid_recovery_snapshot_name};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// Validated metadata stored next to one recovery snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryMetadata {
    pub id: String,
    #[serde(rename = "originalProjectUrl")]
    pub original_project_url: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(rename = "savedAt")]
    pub saved_at: String,
    #[serde(rename = "snapshotFile")]
    pub snapshot_file: String,
}

/// Validation result for a recovery metadata file discovered on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryInspection {
    pub metadata: RecoveryMetadata,
    pub status: String,
}

fn text(value: Option<&Value>) -> String {
    value.and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn number(bytes: &[u8], start: usize, length: usize) -> Option<u32> {
    let digits = bytes.get(start..start.checked_add(length)?)?;
    if digits.iter().any(|byte| !byte.is_ascii_digit()) {
        return None;
    }
    digits.iter().try_fold(0_u32, |value, digit| {
        value.checked_mul(10)?.checked_add(u32::from(digit - b'0'))
    })
}

fn leap_year(year: u32) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

fn valid_iso_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 19
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return false;
    }
    let Some(year) = number(bytes, 0, 4) else {
        return false;
    };
    let Some(month) = number(bytes, 5, 2) else {
        return false;
    };
    let Some(day) = number(bytes, 8, 2) else {
        return false;
    };
    let Some(hour) = number(bytes, 11, 2) else {
        return false;
    };
    let Some(minute) = number(bytes, 14, 2) else {
        return false;
    };
    let Some(second) = number(bytes, 17, 2) else {
        return false;
    };
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap_year(year) => 29,
        2 => 28,
        _ => return false,
    };
    if year == 0 || day == 0 || day > days || hour > 23 || minute > 59 || second > 59 {
        return false;
    }

    let mut index = 19;
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        let fraction_start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == fraction_start {
            return false;
        }
    }
    if index == bytes.len() {
        return true;
    }
    if bytes.get(index) == Some(&b'Z') {
        return index + 1 == bytes.len();
    }
    if !matches!(bytes.get(index), Some(b'+') | Some(b'-')) || index + 6 != bytes.len() {
        return false;
    }
    bytes.get(index + 3) == Some(&b':')
        && number(bytes, index + 1, 2).is_some_and(|hour| hour <= 23)
        && number(bytes, index + 4, 2).is_some_and(|minute| minute <= 59)
}

fn snapshot_file_name(id: &str, metadata: &Map<String, Value>) -> String {
    let file_name = metadata
        .get("snapshotFile")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{id}.aviqtl"));
    if valid_recovery_snapshot_name(id, &file_name) {
        file_name
    } else {
        String::new()
    }
}

fn inspect_metadata(id: &str, metadata: &Map<String, Value>) -> Map<String, Value> {
    let metadata_id = text(metadata.get("id"));
    let saved_at = text(metadata.get("savedAt"));
    let snapshot_file = snapshot_file_name(id, metadata);
    let status = if !valid_recovery_id(id) {
        "invalid_id"
    } else if metadata_id != id {
        "mismatched_id"
    } else if snapshot_file.is_empty() {
        "invalid_snapshot"
    } else if !valid_iso_timestamp(&saved_at) {
        "invalid_timestamp"
    } else {
        "ok"
    };
    json!({
        "id": id,
        "originalProjectUrl": text(metadata.get("originalProjectUrl")),
        "displayName": text(metadata.get("displayName")),
        "savedAt": saved_at,
        "snapshotFile": snapshot_file,
        "status": status,
    })
    .as_object()
    .cloned()
    .expect("inspection is an object")
}

fn metadata_document(
    id: &str,
    original_project_url: &str,
    display_name: &str,
    saved_at: &str,
    snapshot_file: &str,
) -> Option<Map<String, Value>> {
    if !valid_recovery_id(id)
        || !valid_recovery_snapshot_name(id, snapshot_file)
        || !valid_iso_timestamp(saved_at)
    {
        return None;
    }
    json!({
        "id": id,
        "originalProjectUrl": original_project_url,
        "displayName": display_name,
        "savedAt": saved_at,
        "snapshotFile": snapshot_file,
    })
    .as_object()
    .cloned()
}

fn recovery_id_from_snapshot(file_name: &str) -> String {
    let Some(id) = file_name.get(..36) else {
        return String::new();
    };
    if valid_recovery_snapshot_name(id, file_name) {
        id.to_owned()
    } else {
        String::new()
    }
}

/// Reports whether a recovery identifier can safely own files in the recovery directory.
pub fn recovery_id_is_valid(id: &str) -> bool {
    valid_recovery_id(id)
}

/// Reports whether a snapshot file name is canonically owned by a recovery identifier.
pub fn recovery_snapshot_name_is_valid(id: &str, file_name: &str) -> bool {
    valid_recovery_snapshot_name(id, file_name)
}

/// Extracts the recovery identifier that canonically owns a snapshot file name.
pub fn recovery_id_from_snapshot_name(file_name: &str) -> Option<String> {
    let id = recovery_id_from_snapshot(file_name);
    (!id.is_empty()).then_some(id)
}

/// Builds metadata after validating its identifier, timestamp, and snapshot ownership.
pub fn build_recovery_metadata(
    id: &str,
    original_project_url: &str,
    display_name: &str,
    saved_at: &str,
    snapshot_file: &str,
) -> Option<RecoveryMetadata> {
    let document = metadata_document(
        id,
        original_project_url,
        display_name,
        saved_at,
        snapshot_file,
    )?;
    serde_json::from_value(Value::Object(document)).ok()
}

/// Parses and validates metadata against the identifier encoded by its file name.
pub fn inspect_recovery_metadata(id: &str, bytes: &[u8]) -> Option<RecoveryInspection> {
    let metadata = serde_json::from_slice::<Value>(bytes)
        .ok()?
        .as_object()
        .cloned()?;
    let inspection = inspect_metadata(id, &metadata);
    Some(RecoveryInspection {
        metadata: RecoveryMetadata {
            id: text(inspection.get("id")),
            original_project_url: text(inspection.get("originalProjectUrl")),
            display_name: text(inspection.get("displayName")),
            saved_at: text(inspection.get("savedAt")),
            snapshot_file: text(inspection.get("snapshotFile")),
        },
        status: text(inspection.get("status")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_validate_calendar_fraction_and_offsets() {
        assert!(valid_iso_timestamp("2026-09-04T12:30:45.123Z"));
        assert!(valid_iso_timestamp("2024-02-29T12:30:45+08:00"));
        assert!(valid_iso_timestamp("2026-09-04T12:30:45"));
        assert!(!valid_iso_timestamp("2025-02-29T12:30:45Z"));
        assert!(!valid_iso_timestamp("2026-09-04T25:30:45Z"));
        assert!(!valid_iso_timestamp("2026-09-04T12:30:45."));
    }

    #[test]
    fn metadata_rules_build_inspect_and_preserve_legacy_snapshot_fallback() {
        let id = "01234567-89ab-cdef-0123-456789abcdef";
        let generation = "fedcba98-7654-3210-fedc-ba9876543210";
        let snapshot = format!("{id}-{generation}.aviqtl");
        let metadata = metadata_document(
            id,
            "file:///project.aviqtl",
            "Project",
            "2026-09-04T12:30:45.123Z",
            &snapshot,
        )
        .expect("valid metadata");
        let inspection = inspect_metadata(id, &metadata);
        assert_eq!(text(inspection.get("status")), "ok");
        assert_eq!(text(inspection.get("snapshotFile")), snapshot);

        let legacy = json!({
            "id": id,
            "displayName": "Legacy",
            "savedAt": "2026-09-04T12:30:45Z"
        });
        let inspection = inspect_metadata(id, legacy.as_object().expect("fixture"));
        assert_eq!(text(inspection.get("status")), "ok");
        assert_eq!(text(inspection.get("snapshotFile")), format!("{id}.aviqtl"));

        let mismatch = inspect_metadata(
            id,
            json!({
                "id": generation,
                "savedAt": "2026-09-04T12:30:45Z"
            })
            .as_object()
            .expect("fixture"),
        );
        assert_eq!(text(mismatch.get("status")), "mismatched_id");
    }

    #[test]
    fn snapshot_name_extraction_rejects_unowned_and_malformed_files() {
        let id = "01234567-89ab-cdef-0123-456789abcdef";
        let generation = "fedcba98-7654-3210-fedc-ba9876543210";
        assert_eq!(recovery_id_from_snapshot(&format!("{id}.aviqtl")), id);
        assert_eq!(
            recovery_id_from_snapshot(&format!("{id}-{generation}.aviqtl")),
            id
        );
        assert!(recovery_id_from_snapshot(&format!("../{id}.aviqtl")).is_empty());
        assert!(recovery_id_from_snapshot("not-a-recovery.aviqtl").is_empty());
    }
}
