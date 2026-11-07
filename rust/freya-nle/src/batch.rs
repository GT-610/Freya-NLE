//! The batch-output queue: queued exports, their results, and persistence.
//!
//! AviUtl2 lets a user register several exports and run them back to back. This
//! module owns the queue itself — ordering, results, the rows the export window
//! shows — while `export.rs` drives the actual `ExportManager` one item at a
//! time. Keeping the state here makes the ordering rules testable without a GPU
//! or an event loop.

use freya_nle_export::{ExportImageFormat, ExportMode, ExportRequest};
use serde_json::{Map, Value, json};
use slint::StandardListViewItem;
use std::path::PathBuf;

/// How many queued exports one batch may hold.
pub(super) const MAX_BATCH_ITEMS: usize = 64;

const MAX_BATCH_LABEL_CHARS: usize = 120;

/// What happened to one queued item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum BatchOutcome {
    Succeeded,
    Failed(String),
}

/// One queued export with the settings it was registered with.
#[derive(Debug, Clone)]
pub(super) struct BatchItem {
    pub(super) label: String,
    /// The scene this item exports; the runner switches to it first.
    pub(super) scene: i32,
    pub(super) request: ExportRequest,
    pub(super) outcome: Option<BatchOutcome>,
}

impl BatchItem {
    /// A row label that names the file the export writes.
    pub(super) fn display_label(&self) -> String {
        let target = self.request.output_path.display().to_string();
        if self.label.is_empty() {
            target
        } else {
            format!("{} → {target}", self.label)
        }
    }
}

/// The queue plus what the runner is doing with it.
#[derive(Debug, Default)]
pub(super) struct BatchQueue {
    items: Vec<BatchItem>,
    /// Index of the item the export manager is working on.
    running: Option<usize>,
    /// Where to continue after a pause; `Some` also means "paused".
    paused_after: Option<usize>,
    /// Set while an item runs, so it finishes before the queue stops.
    pause_requested: bool,
    /// The scene the user was on when the batch started, restored afterwards.
    restore_scene: Option<i32>,
    /// The line the export window shows under the batch list.
    message: String,
}

impl BatchQueue {
    pub(super) fn items(&self) -> &[BatchItem] {
        &self.items
    }

    pub(super) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub(super) fn is_running(&self) -> bool {
        self.running.is_some()
    }

    pub(super) fn is_paused(&self) -> bool {
        self.paused_after.is_some()
    }

    pub(super) fn running_index(&self) -> Option<usize> {
        self.running
    }

    pub(super) fn pause_requested(&self) -> bool {
        self.pause_requested
    }

    pub(super) fn request_pause(&mut self) {
        self.pause_requested = true;
    }

    pub(super) fn message(&self) -> &str {
        &self.message
    }

    pub(super) fn set_message(&mut self, message: impl Into<String>) {
        self.message = message.into();
    }

    /// Appends one item and returns its index, or `None` when the queue is full.
    pub(super) fn push(
        &mut self,
        label: String,
        scene: i32,
        request: ExportRequest,
    ) -> Option<usize> {
        if self.items.len() >= MAX_BATCH_ITEMS {
            return None;
        }
        let label = truncate_label(&label);
        self.items.push(BatchItem {
            label,
            scene,
            request,
            outcome: None,
        });
        Some(self.items.len() - 1)
    }

    /// Remembers the scene to return to once the queue is done.
    pub(super) fn remember_scene(&mut self, scene: i32) {
        self.restore_scene = Some(scene);
    }

    /// Takes the scene to restore, if a batch ran.
    pub(super) fn take_restore_scene(&mut self) -> Option<i32> {
        self.restore_scene.take()
    }

    pub(super) fn remove(&mut self, index: usize) -> bool {
        if index >= self.items.len() || self.is_running() {
            return false;
        }
        self.items.remove(index);
        true
    }

    pub(super) fn clear(&mut self) -> bool {
        if self.is_running() {
            return false;
        }
        self.items.clear();
        self.paused_after = None;
        true
    }

    /// Moves one item by `delta` rows and returns its new index.
    pub(super) fn move_by(&mut self, index: usize, delta: i32) -> Option<usize> {
        if self.is_running() || index >= self.items.len() {
            return None;
        }
        let target = index.checked_add_signed(delta as isize)?;
        if target >= self.items.len() {
            return None;
        }
        self.items.swap(index, target);
        Some(target)
    }

    /// Marks the item at `index` as the running one.
    pub(super) fn begin(&mut self, index: usize) -> bool {
        if index >= self.items.len() {
            return false;
        }
        self.running = Some(index);
        self.paused_after = None;
        self.pause_requested = false;
        self.items[index].outcome = None;
        true
    }

    pub(super) fn finish_running(&mut self, outcome: BatchOutcome) -> Option<usize> {
        let index = self.running.take()?;
        if let Some(item) = self.items.get_mut(index) {
            item.outcome = Some(outcome);
        }
        Some(index)
    }

    pub(super) fn stop(&mut self) {
        self.running = None;
        self.paused_after = None;
        self.pause_requested = false;
    }

    /// The scene of an item, used to switch before it starts.
    pub(super) fn scene_of(&self, index: usize) -> Option<i32> {
        self.items.get(index).map(|item| item.scene)
    }

    pub(super) fn pause_after(&mut self, next: usize) {
        self.running = None;
        self.paused_after = Some(next);
        self.pause_requested = false;
    }

    pub(super) fn resume(&mut self) -> Option<usize> {
        let next = self.paused_after.take()?;
        self.running = Some(next);
        self.pause_requested = false;
        self.items.get_mut(next)?.outcome = None;
        Some(next)
    }

    /// The counts shown when the queue finishes.
    pub(super) fn summary(&self) -> (usize, usize) {
        let succeeded = self
            .items
            .iter()
            .filter(|item| matches!(item.outcome, Some(BatchOutcome::Succeeded)))
            .count();
        let failed = self
            .items
            .iter()
            .filter(|item| matches!(item.outcome, Some(BatchOutcome::Failed(_))))
            .count();
        (succeeded, failed)
    }

    /// The rows the batch list shows, in queue order.
    pub(super) fn rows(&self) -> Vec<StandardListViewItem> {
        self.items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let status = match &item.outcome {
                    None if self.running == Some(index) => " · running",
                    None => "",
                    Some(BatchOutcome::Succeeded) => " · done",
                    Some(BatchOutcome::Failed(_)) => " · failed",
                };
                StandardListViewItem::from(slint::format!(
                    "{}. {}{status}",
                    index + 1,
                    item.display_label()
                ))
            })
            .collect()
    }
}

fn truncate_label(label: &str) -> String {
    let label = label.trim();
    if label.chars().count() <= MAX_BATCH_LABEL_CHARS {
        return label.to_owned();
    }
    label
        .chars()
        .take(MAX_BATCH_LABEL_CHARS)
        .collect::<String>()
}

/// Serializes the queue's items for the settings file.
pub(super) fn to_json(queue: &BatchQueue) -> Value {
    Value::Array(
        queue
            .items()
            .iter()
            .map(|item| {
                json!({
                    "label": item.label,
                    "scene": item.scene,
                    "mode": mode_name(item.request.mode),
                    "outputPath": item.request.output_path.to_string_lossy(),
                    "imageFormat": image_format_name(item.request.image_format),
                    "videoCodec": item.request.video_codec,
                    "crf": item.request.crf,
                    "bitrateMbps": item.request.bitrate_mbps,
                    "preset": item.request.preset,
                    "profile": item.request.profile,
                    "audioCodec": item.request.audio_codec,
                    "audioBitrateKbps": item.request.audio_bitrate_kbps,
                    "fullRange": item.request.full_range,
                    "startFrame": item.request.start_frame,
                    "endFrame": item.request.end_frame,
                })
            })
            .collect(),
    )
}

/// Rebuilds the queue from the settings file, dropping entries that do not
/// parse instead of failing the whole batch.
pub(super) fn from_json(value: &Value) -> BatchQueue {
    let mut queue = BatchQueue::default();
    let Some(items) = value.as_array() else {
        return queue;
    };
    for item in items {
        if let Some((label, scene, request)) = item_from_json(item)
            && queue.push(label, scene, request).is_none()
        {
            break;
        }
    }
    queue
}

fn item_from_json(value: &Value) -> Option<(String, i32, ExportRequest)> {
    let object = value.as_object()?;
    let output_path = object.get("outputPath")?.as_str()?.trim();
    if output_path.is_empty() {
        return None;
    }
    // Checked: a stored value past `i32` would otherwise wrap (`4294967306`
    // becomes `10`) and export the wrong frames.
    let number = |key: &str, fallback: i32| {
        object
            .get(key)
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok())
            .unwrap_or(fallback)
    };
    let string = |key: &str, fallback: &str| {
        object
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or(fallback)
            .to_owned()
    };
    let label = object
        .get("label")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let scene = object
        .get("scene")
        .and_then(Value::as_i64)
        .and_then(|scene| i32::try_from(scene).ok())
        .unwrap_or(1);
    let full_range = object
        .get("fullRange")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    // A frame field that cannot be represented must not be replaced by 0: with
    // `fullRange: false` the export uses the stored range, so a wrapped value
    // would silently export frames the entry never named.
    if !full_range {
        for key in ["startFrame", "endFrame"] {
            let Some(value) = object.get(key) else {
                continue;
            };
            // A present value has to be an integer that fits: `10.5`, `"10"`
            // and `1e10` all fail `as_i64`, and falling back to the default
            // would export from frame 0 — exactly what this guard prevents.
            let stored = value.as_i64()?;
            i32::try_from(stored).ok()?;
        }
    }
    let request = ExportRequest {
        mode: match object.get("mode").and_then(Value::as_str) {
            Some("image-sequence") => ExportMode::ImageSequence,
            _ => ExportMode::Video,
        },
        output_path: PathBuf::from(output_path),
        image_format: match object.get("imageFormat").and_then(Value::as_str) {
            Some("jpeg") => ExportImageFormat::Jpeg,
            _ => ExportImageFormat::Png,
        },
        video_codec: string("videoCodec", "libx264"),
        crf: object
            .get("crf")
            .and_then(Value::as_i64)
            .map(|value| value as i32),
        bitrate_mbps: number("bitrateMbps", 15),
        preset: string("preset", "medium"),
        profile: string("profile", ""),
        audio_codec: string("audioCodec", "aac"),
        audio_bitrate_kbps: number("audioBitrateKbps", 192),
        full_range,
        start_frame: number("startFrame", 0),
        end_frame: number("endFrame", 0),
    };
    Some((label, scene, request))
}

/// Derives a per-scene output path from the one on the export window.
///
/// A video keeps its extension (`clip.mkv` becomes `clip_scene.mkv`); an image
/// sequence becomes a sibling folder, because the sequence writes many files.
pub(super) fn scene_output_path(
    base: &std::path::Path,
    scene: &str,
    mode: ExportMode,
) -> std::path::PathBuf {
    let safe = scene
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
                )
            {
                '_'
            } else {
                character
            }
        })
        .take(40)
        .collect::<String>();
    let safe = safe.trim().trim_matches('.').to_owned();
    let safe = if safe.is_empty() {
        "scene".to_owned()
    } else {
        safe
    };
    let stem = base
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "export".to_owned());
    let name = format!("{stem}_{safe}");
    match mode {
        ExportMode::Video => {
            // Appending the extension by text, not `set_extension`: a scene
            // name may itself contain a period (`Scene.v1`), and replacing the
            // suffix after it produced `clip_Scene.mkv`, which collides with a
            // real `Scene`.
            let Some(extension) = base.extension().and_then(|value| value.to_str()) else {
                return base.with_file_name(name);
            };
            base.with_file_name(format!("{name}.{extension}"))
        }
        ExportMode::ImageSequence => base.with_file_name(name),
    }
}

fn mode_name(mode: ExportMode) -> &'static str {
    match mode {
        ExportMode::Video => "video",
        ExportMode::ImageSequence => "image-sequence",
    }
}

fn image_format_name(format: ExportImageFormat) -> &'static str {
    match format {
        ExportImageFormat::Png => "png",
        ExportImageFormat::Jpeg => "jpeg",
    }
}

/// A settings-shaped map holding just the batch keys.
pub(super) fn settings_entry(queue: &BatchQueue, completion_index: i32) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("exportBatch".to_owned(), to_json(queue));
    map.insert("exportBatchCompletion".to_owned(), json!(completion_index));
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(path: &str, start: i32, end: i32) -> ExportRequest {
        ExportRequest {
            mode: ExportMode::Video,
            output_path: PathBuf::from(path),
            image_format: ExportImageFormat::Png,
            video_codec: "libx264".to_owned(),
            crf: Some(20),
            bitrate_mbps: 15,
            preset: "medium".to_owned(),
            profile: String::new(),
            audio_codec: "aac".to_owned(),
            audio_bitrate_kbps: 192,
            full_range: false,
            start_frame: start,
            end_frame: end,
        }
    }

    #[test]
    fn items_move_and_remove_without_touching_the_running_item() {
        let mut queue = BatchQueue::default();
        queue.push("first".to_owned(), 1, request("/tmp/a.mkv", 0, 10));
        queue.push("second".to_owned(), 1, request("/tmp/b.mkv", 0, 10));
        queue.push("third".to_owned(), 1, request("/tmp/c.mkv", 0, 10));

        assert_eq!(queue.move_by(2, -1), Some(1));
        assert_eq!(queue.items()[1].label, "third");
        assert_eq!(queue.move_by(0, -1), None, "the first row cannot move up");
        assert_eq!(queue.move_by(0, 5), None, "the last row cannot move down");

        assert!(queue.begin(0));
        assert!(!queue.remove(0), "a running batch cannot be edited");
        assert!(!queue.clear());
        assert!(!queue.move_by(0, 1).is_some());
    }

    #[test]
    fn finishing_an_item_records_its_result_and_stops_the_run() {
        let mut queue = BatchQueue::default();
        queue.push("first".to_owned(), 1, request("/tmp/a.mkv", 0, 10));
        queue.push("second".to_owned(), 1, request("/tmp/b.mkv", 0, 10));
        assert!(queue.begin(0));
        assert_eq!(
            queue.finish_running(BatchOutcome::Succeeded),
            Some(0),
            "the finished index is reported so the runner can advance"
        );
        assert!(!queue.is_running());
        assert_eq!(queue.summary(), (1, 0));
        let rows = queue.rows();
        assert!(rows[0].text.contains("done"));
        assert!(!rows[1].text.contains("done"));
    }

    #[test]
    fn pausing_resumes_at_the_next_item() {
        let mut queue = BatchQueue::default();
        queue.push("first".to_owned(), 1, request("/tmp/a.mkv", 0, 10));
        queue.push("second".to_owned(), 1, request("/tmp/b.mkv", 0, 10));
        queue.begin(0);
        queue.finish_running(BatchOutcome::Failed("codec missing".to_owned()));
        queue.pause_after(1);
        assert!(queue.is_paused());
        assert_eq!(queue.summary(), (0, 1));
        assert_eq!(queue.resume(), Some(1));
        assert!(!queue.is_paused());
        assert!(queue.is_running());
    }

    #[test]
    fn the_queue_round_trips_through_settings_json() {
        let mut queue = BatchQueue::default();
        queue.push(
            "scene 1".to_owned(),
            2,
            request("/tmp/out/scene1.mkv", 10, 90),
        );
        let mut image = request("/tmp/out/frames", 0, 30);
        image.mode = ExportMode::ImageSequence;
        image.image_format = ExportImageFormat::Jpeg;
        image.crf = None;
        queue.push("frames".to_owned(), 1, image);

        let restored = from_json(&to_json(&queue));
        assert_eq!(restored.items().len(), 2);
        assert_eq!(restored.items()[0].label, "scene 1");
        assert_eq!(restored.items()[0].scene, 2);
        assert_eq!(restored.items()[0].request.start_frame, 10);
        assert_eq!(restored.items()[1].request.mode, ExportMode::ImageSequence);
        assert_eq!(
            restored.items()[1].request.image_format,
            ExportImageFormat::Jpeg
        );
        assert_eq!(restored.items()[1].request.crf, None);

        let broken = from_json(&json!([{"label": "no path"}, 7, {"outputPath": ""}]));
        assert!(broken.is_empty(), "entries without a path are dropped");
    }

    /// A stored frame that does not fit `i32` must not become 0: with a fixed
    /// range the export uses it, so the entry would export frames it never
    /// named. The whole item is dropped instead.
    #[test]
    fn out_of_range_frames_drop_a_fixed_range_item() {
        let entries = serde_json::json!([
            {
                "label": "wrapped",
                "scene": 1,
                "outputPath": "/tmp/out/wrapped.mkv",
                "mode": "video",
                "fullRange": false,
                "startFrame": 4_294_967_306_i64,
                "endFrame": 20
            },
            {
                "label": "valid",
                "scene": 1,
                "outputPath": "/tmp/out/valid.mkv",
                "mode": "video",
                "fullRange": false,
                "startFrame": 10,
                "endFrame": 20
            }
        ]);
        let queue = from_json(&entries);
        assert_eq!(queue.items().len(), 1, "the wrapped entry is dropped");
        assert_eq!(queue.items()[0].request.start_frame, 10);

        // A present but non-integer frame is just as unusable: the default
        // would make it export from frame 0.
        for bad in [serde_json::json!(10.5), serde_json::json!("10")] {
            let entries = serde_json::json!([{
                "label": "fractional",
                "scene": 1,
                "outputPath": "/tmp/out/fractional.mkv",
                "mode": "video",
                "fullRange": false,
                "startFrame": bad,
                "endFrame": 20
            }]);
            assert!(
                from_json(&entries).items().is_empty(),
                "a non-integer startFrame drops the item"
            );
        }

        // A full-range entry ignores the frame fields, so it keeps its place.
        let entries = serde_json::json!([{
            "label": "full",
            "scene": 1,
            "outputPath": "/tmp/out/full.mkv",
            "mode": "video",
            "fullRange": true,
            "startFrame": 4_294_967_306_i64,
            "endFrame": 20
        }]);
        assert_eq!(from_json(&entries).items().len(), 1);
    }

    #[test]
    fn per_scene_paths_keep_the_extension_and_sanitize_the_name() {
        let video = scene_output_path(
            std::path::Path::new("/tmp/out/harbor.mkv"),
            "Scene 1/2",
            ExportMode::Video,
        );
        assert_eq!(video, std::path::Path::new("/tmp/out/harbor_Scene 1_2.mkv"));

        let sequence = scene_output_path(
            std::path::Path::new("/tmp/out/frames"),
            "タイトル",
            ExportMode::ImageSequence,
        );
        assert_eq!(sequence, std::path::Path::new("/tmp/out/frames_タイトル"));

        let empty = scene_output_path(
            std::path::Path::new("/tmp/out/clip.mkv"),
            " ... ",
            ExportMode::Video,
        );
        assert_eq!(empty, std::path::Path::new("/tmp/out/clip_scene.mkv"));
    }

    #[test]
    fn the_queue_remembers_the_scene_to_return_to() {
        let mut queue = BatchQueue::default();
        queue.push("first".to_owned(), 3, request("/tmp/a.mkv", 0, 10));
        assert_eq!(queue.scene_of(0), Some(3));
        assert_eq!(queue.take_restore_scene(), None, "no batch ran yet");
        queue.remember_scene(7);
        assert_eq!(queue.take_restore_scene(), Some(7));
        assert_eq!(queue.take_restore_scene(), None, "restoring consumes it");
    }

    #[test]
    fn the_queue_refuses_more_items_than_the_cap() {
        let mut queue = BatchQueue::default();
        for index in 0..MAX_BATCH_ITEMS {
            assert!(
                queue
                    .push(format!("item {index}"), 1, request("/tmp/a.mkv", 0, 10))
                    .is_some()
            );
        }
        assert_eq!(
            queue.push("overflow".to_owned(), 1, request("/tmp/b.mkv", 0, 10)),
            None
        );
        assert_eq!(queue.items().len(), MAX_BATCH_ITEMS);
    }
}
