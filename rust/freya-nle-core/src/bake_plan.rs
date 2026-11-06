use crate::expression;
use crate::keyframe_document::{evaluate_resolved_track, parse_hex_color, resolve_track};
use crate::policy::{PlaybackMode, playback_mode, resolve_video_time};
use crate::project::{
    AudioPluginDocument, ClipDocument, EffectDocument, ExtraFields, LayerSettingDocument,
    ProjectDocument,
};
use crate::text_control::{TextBaseStyle, TextRun};
use crate::timeline::{bake_audio, bake_render};
use crate::types::{AudioBakeInput, AudioBakeOutput, RenderBakeInput};
use serde_json::Value;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::error::Error;
use std::fmt::{Display, Formatter};

const FRAME_BUCKET_SIZE: i32 = 120;
const DEFAULT_SPEED_PERCENT: f32 = 100.0;

/// Error returned when a scene render plan cannot be built from a project snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneRenderPlanError {
    /// The requested scene does not exist in the project document.
    SceneNotFound(i32),
}

impl Display for SceneRenderPlanError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SceneNotFound(scene_id) => write!(formatter, "scene #{scene_id} does not exist"),
        }
    }
}

impl Error for SceneRenderPlanError {}

/// Rust-owned cached evaluator for one scene in a typed project document.
pub struct SceneRenderPlan {
    plan: BakePlan,
}

impl SceneRenderPlan {
    /// Builds the cached keyframe and timeline plan for one scene.
    pub fn from_document(
        document: &ProjectDocument,
        scene_id: i32,
    ) -> Result<Self, SceneRenderPlanError> {
        Ok(Self {
            plan: BakePlan::from_document(document, scene_id)?,
        })
    }

    /// Evaluates active render and audio layers at an absolute scene frame.
    pub fn evaluate(&mut self, current_frame: i32) -> SceneFramePlan {
        self.plan.compute_frame(EvaluationKey { current_frame })
    }

    /// Returns the number of effects cached by this plan.
    pub fn effect_count(&self) -> usize {
        self.plan.effect_count
    }
}

/// Fully evaluated state needed to render and mix one scene frame.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneFramePlan {
    pub frame: i32,
    pub layers: Vec<RenderLayerPlan>,
    pub audio: Vec<AudioLayerPlan>,
    pub camera: Option<CameraRenderPlan>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraRenderPlan {
    pub position_x: f32,
    pub position_y: f32,
    pub position_z: f32,
    pub target_x: f32,
    pub target_y: f32,
    pub target_z: f32,
    pub roll_degrees: f32,
    pub field_of_view_degrees: f32,
}

/// Bound for one evaluated parameter before the `f64` to `f32` cast.
///
/// Far outside anything the interface can set, but small enough that the cast
/// cannot overflow to infinity.
const MAX_EVALUATED_VALUE: f64 = 1.0e30;

/// Evaluated transform values for a render layer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EvaluatedTransform {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub rotation_x: f32,
    pub rotation_y: f32,
    pub rotation_z: f32,
    pub scale_percent: f32,
    pub opacity: f32,
    /// Aspect ratio from the standard drawing: `-1.0..=1.0`, positive squeezes
    /// horizontally and negative squeezes vertically, 0 leaves it alone.
    ///
    /// These are the units AviUtl2's Lua reference documents for `obj.aspect`
    /// and the ones the compositor's `LayerTransform` already computed with
    /// (`1.0 + aspect` / `1.0 - aspect`), so the value travels without a second
    /// conversion. A percentage here would make the twice-converted default of
    /// 100 stretch every object 101x wide.
    pub aspect: f32,
    /// Pivot the object rotates and scales around (`cx`/`cy`/`cz`).
    pub pivot_x: f32,
    pub pivot_y: f32,
    pub pivot_z: f32,
}

impl Default for EvaluatedTransform {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            rotation_x: 0.0,
            rotation_y: 0.0,
            rotation_z: 0.0,
            scale_percent: 100.0,
            opacity: 1.0,
            aspect: 0.0,
            pivot_x: 0.0,
            pivot_y: 0.0,
            pivot_z: 0.0,
        }
    }
}

/// Parameters for one enabled effect after keyframe evaluation.
#[derive(Debug, Clone, PartialEq)]
pub struct EvaluatedEffect {
    pub effect_index: usize,
    pub id: String,
    pub params: BTreeMap<String, Value>,
}

/// Kind of media source attached to a render layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
}

/// Evaluated media playback state. Video time is resolved once source FPS is known.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaPlaybackPlan {
    pub kind: MediaKind,
    pub path: String,
    pub relative_frame: i32,
    pub direct_mode: bool,
    pub start_frame: f64,
    pub direct_frame: f64,
    pub speed_percent: f64,
    pub opacity: f32,
    fallback_fps: f64,
}

impl MediaPlaybackPlan {
    /// Resolves the requested source timestamp using decoder metadata when available.
    pub fn timestamp_seconds(&self, source_fps: f64) -> f64 {
        if self.kind == MediaKind::Image {
            return 0.0;
        }
        let source_fps = if source_fps.is_finite() && source_fps > 0.0 {
            source_fps
        } else {
            self.fallback_fps
        };
        resolve_video_time(
            self.relative_frame,
            source_fps,
            self.direct_mode,
            self.direct_frame,
            self.start_frame,
            self.speed_percent,
        )
    }
}

/// Evaluated visual state for one active timeline clip.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderLayerPlan {
    pub clip_id: i32,
    pub clip_type: String,
    pub timeline_layer: i32,
    pub start_frame: i32,
    pub duration_frames: i32,
    pub relative_frame: i32,
    pub clip_by_upper_object: bool,
    pub transform: EvaluatedTransform,
    pub effects: Vec<EvaluatedEffect>,
    pub media: Option<MediaPlaybackPlan>,
    pub shape: Option<ShapeRenderPlan>,
    pub text: Option<TextRenderPlan>,
    pub procedural: Option<ProceduralObjectRenderPlan>,
    pub nested_scene: Option<NestedSceneRenderPlan>,
}

/// Evaluated playback state for a scene object embedded in another scene.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NestedSceneRenderPlan {
    pub target_scene_id: i32,
    pub target_frame: i32,
    pub opacity: f32,
}

/// Procedural shape family used by AviQtl's built-in `rect` object.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ShapeKind {
    #[default]
    Polygon,
    Star,
    Pie,
    Arc,
    Donut,
}

/// Gradient mode used by a procedural shape fill.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ShapeGradientKind {
    #[default]
    Linear,
    Radial,
}

/// Straight-alpha color evaluated from an AviQtl color parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RgbaColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

/// Fully evaluated parameters for AviQtl's built-in procedural shape object.
#[derive(Debug, Clone, PartialEq)]
pub struct ShapeRenderPlan {
    pub kind: ShapeKind,
    pub width: f32,
    pub height: f32,
    pub sides: u32,
    pub sweep_degrees: f32,
    pub corner_radius: f32,
    pub inner_radius_percent: f32,
    pub rotation_degrees: f32,
    pub normalize_vertices: bool,
    pub even_sides_half_step: bool,
    pub edge_padding: f32,
    pub fill_color: RgbaColor,
    pub use_gradient: bool,
    pub gradient_color: RgbaColor,
    pub gradient_kind: ShapeGradientKind,
    pub stroke_color: RgbaColor,
    pub stroke_width: f32,
    pub dash_length: f32,
    pub dash_space: f32,
    pub opacity: f32,
}

/// Horizontal alignment used by AviQtl's built-in text object.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TextAlignment {
    Left,
    Right,
    #[default]
    Center,
    Justify,
}

/// Fully evaluated parameters for AviQtl's built-in text object.
#[derive(Debug, Clone, PartialEq)]
pub struct TextRenderPlan {
    /// The raw content, control characters included. Layout uses `runs`.
    pub content: String,
    /// Styled runs after control characters. When `runs_authoritative` is set
    /// the renderer draws only these runs, even when the list is empty because
    /// a `<c>` directive cleared the text; otherwise it falls back to `content`.
    pub runs: Vec<TextRun>,
    pub runs_authoritative: bool,
    pub font_family: String,
    pub font_size: f32,
    pub bold: bool,
    pub italic: bool,
    pub letter_spacing: f32,
    pub line_spacing: f32,
    pub alignment: TextAlignment,
    pub color: RgbaColor,
    pub outline_enabled: bool,
    pub outline_color: RgbaColor,
    pub outline_width: f32,
    pub shadow_enabled: bool,
    pub shadow_color: RgbaColor,
    pub shadow_offset_x: f32,
    pub shadow_offset_y: f32,
    pub background_enabled: bool,
    pub background_color: RgbaColor,
    pub background_radius: f32,
    pub background_padding_x: f32,
    pub background_padding_y: f32,
    pub opacity: f32,
}

/// CPU-rasterized custom object that mirrors one of the Qt Canvas implementations.
#[derive(Debug, Clone, PartialEq)]
pub enum ProceduralObjectRenderPlan {
    TrackLine(TrackLineRenderPlan),
    ParticleField(ParticleFieldRenderPlan),
    RadialLines(RadialLinesRenderPlan),
    LensFlare(LensFlareRenderPlan),
}

impl ProceduralObjectRenderPlan {
    pub fn opacity(&self) -> f32 {
        match self {
            Self::TrackLine(plan) => plan.opacity,
            Self::ParticleField(plan) => plan.opacity,
            Self::RadialLines(plan) => plan.opacity,
            Self::LensFlare(plan) => plan.opacity,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrackLineRenderPlan {
    pub width: f32,
    pub height: f32,
    pub start_x: f32,
    pub start_y: f32,
    pub end_x: f32,
    pub end_y: f32,
    pub line_width: f32,
    pub dash_length: f32,
    pub dash_space: f32,
    pub arrow: bool,
    pub color: RgbaColor,
    pub opacity: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParticleFieldRenderPlan {
    pub width: f32,
    pub height: f32,
    pub count: u32,
    pub speed: f32,
    pub particle_size: f32,
    pub spread: f32,
    pub seed: i32,
    pub color: RgbaColor,
    pub opacity: f32,
    pub relative_frame: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RadialLinesRenderPlan {
    pub width: f32,
    pub height: f32,
    pub line_count: u32,
    pub min_length: f32,
    pub max_length: f32,
    pub thickness: f32,
    pub randomness: f32,
    pub center_x: f32,
    pub center_y: f32,
    pub spin_speed: f32,
    pub seed: i32,
    pub color: RgbaColor,
    pub opacity: f32,
    pub relative_frame: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LensFlareRenderPlan {
    pub width: f32,
    pub height: f32,
    pub center_x: f32,
    pub center_y: f32,
    pub radius: f32,
    pub strength: f32,
    pub ghosts: u32,
    pub color: RgbaColor,
    pub opacity: f32,
}

/// Evaluated audio state for one active audio or video clip.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioLayerPlan {
    pub clip_id: i32,
    pub source_path: Option<String>,
    pub start_frame: i32,
    pub duration_frames: i32,
    pub source_start_time: f32,
    pub playback_speed: f32,
    pub direct_time: f32,
    pub volume: f32,
    pub master_volume: f32,
    pub pan: f32,
    pub fade_in_seconds: f32,
    pub fade_out_seconds: f32,
    pub mute: bool,
    pub solo: bool,
    pub limiter: bool,
    pub direct_mode: bool,
    pub plugins: Vec<EvaluatedAudioPlugin>,
}

/// One enabled or bypassed audio-plugin instance with parameters evaluated for this frame.
#[derive(Debug, Clone, PartialEq)]
pub struct EvaluatedAudioPlugin {
    pub id: String,
    pub enabled: bool,
    pub params: BTreeMap<String, Value>,
    pub extra: ExtraFields,
}

#[cfg(test)]
use serde::Deserialize;
#[cfg(test)]
use serde_json::Map;

#[cfg(test)]
fn default_max_clip_id() -> i32 {
    4096
}

#[cfg(test)]
fn default_scene_width() -> i32 {
    1920
}

#[cfg(test)]
fn default_scene_height() -> i32 {
    1080
}

#[cfg(test)]
fn default_enabled() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg(test)]
struct SceneInput {
    fps: f64,
    #[serde(default = "default_scene_width")]
    width: i32,
    #[serde(default = "default_scene_height")]
    height: i32,
    #[serde(default = "default_max_clip_id")]
    max_clip_id: i32,
    #[serde(default)]
    clips: Vec<ClipInput>,
    #[serde(default)]
    layer_settings: Vec<LayerSettingDocument>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg(test)]
struct ClipInput {
    id: i32,
    #[serde(default)]
    layer: i32,
    #[serde(default)]
    start_frame: i32,
    #[serde(default)]
    duration_frames: i32,
    #[serde(default)]
    clip_by_upper_object: bool,
    #[serde(default, rename = "type")]
    clip_type: String,
    #[serde(default)]
    effects: Vec<EffectInput>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg(test)]
struct EffectInput {
    #[serde(default)]
    id: String,
    /// Display name, which `Effect.Param` references match.
    #[serde(default)]
    name: String,
    /// Unknown fields ride along, including the `expressions` map.
    #[serde(flatten)]
    extra: ExtraFields,
    #[serde(default = "default_enabled")]
    enabled: bool,
    #[serde(default)]
    known: bool,
    #[serde(default)]
    params: BTreeMap<String, Value>,
    #[serde(default)]
    keyframes: BTreeMap<String, Vec<KeyframeInput>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg(test)]
struct KeyframeInput {
    #[serde(default)]
    frame: i32,
    #[serde(default)]
    value: Value,
    #[serde(default = "default_interpolation", rename = "interp")]
    interpolation: String,
    #[serde(default = "default_bzx1")]
    bzx1: f64,
    #[serde(default)]
    bzy1: f64,
    #[serde(default = "default_bzx2")]
    bzx2: f64,
    #[serde(default = "default_bzy2")]
    bzy2: f64,
}

#[cfg(test)]
fn default_interpolation() -> String {
    "linear".to_owned()
}

#[cfg(test)]
fn default_bzx1() -> f64 {
    0.33
}

#[cfg(test)]
fn default_bzx2() -> f64 {
    0.66
}

#[cfg(test)]
fn default_bzy2() -> f64 {
    1.0
}

fn payload(value: &Value) -> &Value {
    value
        .as_object()
        .filter(|object| object.contains_key("$aviqtlType"))
        .and_then(|object| object.get("value"))
        .unwrap_or(value)
}

fn numeric_value(value: &Value) -> Option<f64> {
    let value = payload(value);
    value
        .as_f64()
        .or_else(|| value.as_bool().map(|value| if value { 1.0 } else { 0.0 }))
}

fn qt_double(value: Option<&Value>) -> f64 {
    let Some(value) = value.map(payload) else {
        return 0.0;
    };
    numeric_value(value)
        .or_else(|| value.as_str().and_then(|value| value.parse::<f64>().ok()))
        .unwrap_or(0.0)
}

fn qt_bool(value: Option<&Value>, fallback: bool) -> bool {
    let Some(value) = value.map(payload) else {
        return fallback;
    };
    if let Some(value) = value.as_bool() {
        return value;
    }
    if let Some(value) = value.as_f64() {
        return value != 0.0;
    }
    match value.as_str() {
        Some("true" | "1") => true,
        Some("false" | "0" | "") => false,
        Some(_) => true,
        None => false,
    }
}

fn point_value(point: &Value) -> Option<&Value> {
    point.as_object().and_then(|point| point.get("value"))
}

#[cfg(test)]
fn keyframe_point(keyframe: KeyframeInput) -> Value {
    let mut point = Map::new();
    point.insert("frame".to_owned(), Value::from(keyframe.frame));
    point.insert("value".to_owned(), keyframe.value);
    let custom = keyframe.interpolation == "custom";
    point.insert("interp".to_owned(), Value::String(keyframe.interpolation));
    if custom {
        point.insert(
            "points".to_owned(),
            Value::Array(
                [
                    keyframe.bzx1,
                    keyframe.bzy1,
                    keyframe.bzx2,
                    keyframe.bzy2,
                    1.0,
                    1.0,
                ]
                .into_iter()
                .map(Value::from)
                .collect(),
            ),
        );
    }
    Value::Object(point)
}

struct ResolvedTrack {
    fallback: Value,
    points: Vec<Value>,
    numeric: bool,
}

impl ResolvedTrack {
    #[cfg(test)]
    fn new(keyframes: Vec<KeyframeInput>, fallback: Value, duration: i32) -> Self {
        let mut start = Map::new();
        start.insert("frame".to_owned(), Value::from(0));
        start.insert("value".to_owned(), fallback.clone());
        start.insert("interp".to_owned(), Value::String("none".to_owned()));
        let mut points = Vec::new();
        for keyframe in keyframes {
            let point = keyframe_point(keyframe);
            if point
                .as_object()
                .and_then(|point| point.get("frame"))
                .and_then(Value::as_i64)
                .is_some_and(|frame| frame <= 0)
            {
                start = point.as_object().cloned().unwrap_or_default();
            } else {
                points.push(point);
            }
        }
        let track = Value::Object(Map::from_iter([
            ("start".to_owned(), Value::Object(start)),
            ("points".to_owned(), Value::Array(points)),
        ]));
        Self::from_document_track(&track, fallback, duration)
    }

    fn from_document_track(track: &Value, fallback: Value, duration: i32) -> Self {
        let points = resolve_track(track, &fallback, duration);
        let numeric = points
            .iter()
            .all(|point| point_value(point).and_then(numeric_value).is_some());
        Self {
            fallback,
            points,
            numeric,
        }
    }

    fn evaluate(&self, frame: i32) -> Value {
        evaluate_resolved_track(&self.points, frame, &self.fallback)
    }
}

struct CachedEffect {
    id: String,
    /// The document's display name, which `Effect.Param` references match.
    name: String,
    enabled: bool,
    known: bool,
    fps: f64,
    params: BTreeMap<String, Value>,
    extra: ExtraFields,
    tracks: BTreeMap<String, ResolvedTrack>,
    all_keys: Vec<String>,
    numeric_track_count: usize,
    last_numeric_frame: Option<i32>,
    numeric_values: BTreeMap<String, f64>,
    /// Values an expression resolved to for the current frame, keyed by the
    /// parameter the expression is stored on.
    resolved_expressions: BTreeMap<String, f64>,
    /// Per-parameter reference expressions stored separately from the value,
    /// so the numeric fallback survives when an expression cannot resolve.
    expressions: BTreeMap<String, String>,
}

impl CachedEffect {
    #[cfg(test)]
    fn new(input: EffectInput, duration: i32, fps: f64) -> Self {
        let mut all_keys: BTreeSet<String> = input.params.keys().cloned().collect();
        all_keys.insert("time".to_owned());
        let mut tracks = BTreeMap::new();
        for (name, keyframes) in input.keyframes {
            all_keys.insert(name.clone());
            let fallback = input.params.get(&name).cloned().unwrap_or(Value::Null);
            tracks.insert(name, ResolvedTrack::new(keyframes, fallback, duration));
        }
        let numeric_track_count = tracks.values().filter(|track| track.numeric).count();
        let expressions = expressions_from_extra(&input.extra);
        Self {
            id: input.id,
            name: input.name,
            enabled: input.enabled,
            known: input.known,
            fps,
            params: input.params,
            extra: input.extra,
            tracks,
            all_keys: all_keys.into_iter().collect(),
            numeric_track_count,
            last_numeric_frame: None,
            numeric_values: BTreeMap::new(),
            resolved_expressions: BTreeMap::new(),
            expressions,
        }
    }

    fn from_document(effect: &EffectDocument, duration: i32, fps: f64) -> Self {
        let params = effect
            .params
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut all_keys: BTreeSet<String> = params.keys().cloned().collect();
        all_keys.insert("time".to_owned());
        let mut tracks = BTreeMap::new();
        if let Some(keyframes) = &effect.keyframes {
            for (name, track) in keyframes {
                all_keys.insert(name.clone());
                let fallback = params.get(name).cloned().unwrap_or(Value::Null);
                tracks.insert(
                    name.clone(),
                    ResolvedTrack::from_document_track(track, fallback, duration),
                );
            }
        }
        let numeric_track_count = tracks.values().filter(|track| track.numeric).count();
        let expressions = expressions_from_extra(&effect.extra);
        Self {
            id: effect.id.clone(),
            name: effect.name.clone(),
            enabled: effect.enabled,
            known: true,
            fps,
            params,
            extra: effect.extra.clone(),
            tracks,
            all_keys: all_keys.into_iter().collect(),
            numeric_track_count,
            last_numeric_frame: None,
            numeric_values: BTreeMap::new(),
            resolved_expressions: BTreeMap::new(),
            expressions,
        }
    }

    fn from_audio_plugin(plugin: &AudioPluginDocument, duration: i32, fps: f64) -> Self {
        let params = plugin
            .params
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut all_keys: BTreeSet<String> = params.keys().cloned().collect();
        let mut tracks = BTreeMap::new();
        if let Some(keyframes) = &plugin.keyframes {
            for (name, track) in keyframes {
                all_keys.insert(name.clone());
                let fallback = params.get(name).cloned().unwrap_or(Value::Null);
                tracks.insert(
                    name.clone(),
                    ResolvedTrack::from_document_track(track, fallback, duration),
                );
            }
        }
        let numeric_track_count = tracks.values().filter(|track| track.numeric).count();
        Self {
            id: plugin.id.clone(),
            name: String::new(),
            enabled: plugin.enabled,
            known: true,
            fps,
            params,
            extra: plugin.extra.clone(),
            tracks,
            all_keys: all_keys.into_iter().collect(),
            numeric_track_count,
            last_numeric_frame: None,
            numeric_values: BTreeMap::new(),
            resolved_expressions: BTreeMap::new(),
            expressions: BTreeMap::new(),
        }
    }

    fn prepare_numeric(&mut self, frame: i32) {
        if self.last_numeric_frame == Some(frame) {
            return;
        }
        self.numeric_values.clear();
        self.resolved_expressions.clear();
        self.last_numeric_frame = Some(frame);
        if self.numeric_track_count == 0 {
            return;
        }
        for (name, track) in &self.tracks {
            if !track.numeric {
                continue;
            }
            let value = numeric_value(&track.evaluate(frame)).unwrap_or(0.0);
            self.numeric_values.insert(name.clone(), value);
        }
    }

    fn value(&self, name: &str, frame: i32) -> Value {
        if name == "time" {
            return Value::from(frame);
        }
        if let Some(value) = self.resolved_expressions.get(name) {
            return Value::from(*value);
        }
        let base = if let Some(value) = self.numeric_values.get(name) {
            Value::from(*value)
        } else if let Some(track) = self.tracks.get(name) {
            track.evaluate(frame)
        } else {
            self.params.get(name).cloned().unwrap_or(Value::Null)
        };
        self.expression_value(name, frame, base)
    }

    /// Seconds from the start of the object, which both expression paths use.
    fn time_seconds(&self, frame: i32) -> f64 {
        if self.fps > 0.0 {
            f64::from(frame) / self.fps
        } else {
            0.0
        }
    }

    /// The stored expression for `name`, without its leading `=`.
    fn expression_source(&self, name: &str) -> Option<&str> {
        if let Some(expression) = self.expressions.get(name) {
            return Some(expression);
        }
        // Imported projects may carry the AviUtl 1.x form, an `=` expressed
        // directly in the parameter value.
        let Value::String(expression) = self.params.get(name)? else {
            return None;
        };
        expression::is_expression(expression).then(|| &expression[1..])
    }

    fn expression_names(&self) -> Vec<String> {
        let mut names = self.expressions.keys().cloned().collect::<BTreeSet<_>>();
        names.extend(
            self.params
                .iter()
                .filter(|(_, value)| {
                    matches!(value, Value::String(expression) if expression::is_expression(expression))
                })
                .map(|(name, _)| name.clone()),
        );
        names.into_iter().collect()
    }

    /// The frame value without applying the expression, so `$` and sibling
    /// references read real numbers instead of recursing into the expression.
    fn raw_numeric(&self, name: &str, frame: i32) -> Option<f64> {
        let value = if let Some(value) = self.numeric_values.get(name) {
            Value::from(*value)
        } else if let Some(track) = self.tracks.get(name) {
            track.evaluate(frame)
        } else {
            self.params.get(name).cloned()?
        };
        numeric_value(&value)
    }

    /// Applies an AviUtl `=expression` stored for `name` on top of the keyframe
    /// value, which is what ExEdit exposes to the expression as `value`.
    fn expression_value(&self, name: &str, frame: i32, base: Value) -> Value {
        let Some(Value::String(expression)) = self.params.get(name) else {
            return base;
        };
        if !expression::is_expression(expression) {
            return base;
        }
        let current = numeric_value(&base).unwrap_or(0.0);
        let time_seconds = self.time_seconds(frame);
        match expression::evaluate(&expression[1..], time_seconds, 0, current) {
            Some(value) => Value::from(value),
            // A broken expression keeps the literal fallback rather than
            // dropping the parameter to zero.
            None => base,
        }
    }

    fn float(&self, name: &str, frame: i32) -> f32 {
        // A JSON number is an `f64`, so a hand-edited `1e300` is valid input but
        // becomes `inf` once cast. The compositor cannot use that, so the value
        // is bounded before the cast rather than poisoning the frame.
        qt_double(Some(&self.value(name, frame))).clamp(-MAX_EVALUATED_VALUE, MAX_EVALUATED_VALUE)
            as f32
    }

    fn float_or(&self, name: &str, fallback: f32, frame: i32) -> f32 {
        if !self.params.contains_key(name) && !self.tracks.contains_key(name) {
            return fallback;
        }
        self.float(name, frame)
    }

    /// Resolves a boolean parameter, which an expression can also produce.
    fn flag_or(&self, name: &str, fallback: bool, frame: i32) -> bool {
        if !self.params.contains_key(name) && !self.tracks.contains_key(name) {
            return fallback;
        }
        qt_bool(Some(&self.value(name, frame)), fallback)
    }
}

struct CachedClip {
    clip_id: i32,
    layer: i32,
    start_frame: i32,
    duration_frames: i32,
    end_frame: i32,
    clip_by_upper_object: bool,
    clip_type: String,
    audio_plugins: Vec<CachedEffect>,
    effects: Vec<CachedEffect>,
}

#[derive(Debug, Clone, Copy)]
struct EvaluatedGroupControl {
    layer: i32,
    layer_count: i32,
    x: f64,
    y: f64,
    z: f64,
    scale: f64,
    rotation_x: f64,
    rotation_y: f64,
    rotation_z: f64,
    opacity: f32,
}

impl CachedClip {
    #[cfg(test)]
    fn new(input: ClipInput, fps: f64) -> Self {
        let effects = input
            .effects
            .into_iter()
            .map(|effect| CachedEffect::new(effect, input.duration_frames, fps))
            .collect();
        Self {
            clip_id: input.id,
            layer: input.layer,
            start_frame: input.start_frame,
            duration_frames: input.duration_frames,
            end_frame: input
                .start_frame
                .saturating_add(input.duration_frames.max(0)),
            clip_by_upper_object: input.clip_by_upper_object,
            clip_type: input.clip_type,
            audio_plugins: Vec::new(),
            effects,
        }
    }

    fn from_document(clip: &ClipDocument, fps: f64) -> Self {
        let audio_plugins = clip
            .audio_plugins
            .iter()
            .map(|plugin| CachedEffect::from_audio_plugin(plugin, clip.duration, fps))
            .collect();
        let effects = clip
            .effects
            .iter()
            .map(|effect| CachedEffect::from_document(effect, clip.duration, fps))
            .collect();
        Self {
            clip_id: clip.id,
            layer: clip.layer,
            start_frame: clip.start,
            duration_frames: clip.duration,
            end_frame: clip.start.saturating_add(clip.duration.max(0)),
            clip_by_upper_object: clip.clip_by_upper_object,
            clip_type: clip.clip_type.clone(),
            audio_plugins,
            effects,
        }
    }
}

/// Reads the `expressions` map an effect carries alongside its values.
fn expressions_from_extra(extra: &ExtraFields) -> BTreeMap<String, String> {
    extra
        .get("expressions")
        .and_then(Value::as_object)
        .map(|expressions| {
            expressions
                .iter()
                .filter_map(|(name, value)| {
                    value.as_str().map(|value| (name.clone(), value.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Resolves the reference expressions of every active clip for one frame.
///
/// AviUtl2 lets a parameter read other parameters by name: a bare `X` is
/// another parameter of the same effect, `ぼかし.範囲` another effect of the same
/// object, `above.X` / `layer1.X` / `レイヤー名.X` the same effect on another
/// layer, and `$` the parameter's own name. References become numbers and the
/// rest of the expression is evaluated by the existing Lua path, so both
/// expression styles share one evaluator.
fn resolve_scene_expressions(
    clips: &mut [CachedClip],
    active: &[usize],
    current_frame: i32,
    layer_settings: &BTreeMap<i32, LayerSetting>,
) {
    if clips.is_empty() || active.is_empty() {
        return;
    }
    let scene = ExpressionScene {
        clips,
        active,
        current_frame,
        layer_settings,
    };
    let mut cache: HashMap<(usize, usize, String), Option<f64>> = HashMap::new();
    let mut resolved = Vec::new();
    for &clip_index in active {
        let Some(clip) = scene.clips.get(clip_index) else {
            continue;
        };
        for effect_index in 0..clip.effects.len() {
            for name in scene.clips[clip_index].effects[effect_index].expression_names() {
                let mut visiting = HashSet::new();
                if let Some(value) =
                    scene.resolve(clip_index, effect_index, &name, &mut visiting, &mut cache)
                {
                    resolved.push(((clip_index, effect_index, name), value));
                }
            }
        }
    }
    for ((clip_index, effect_index, name), value) in resolved {
        if let Some(effect) = clips
            .get_mut(clip_index)
            .and_then(|clip| clip.effects.get_mut(effect_index))
        {
            effect.resolved_expressions.insert(name, value);
        }
    }
}

/// The active clips and layer names one frame's references resolve against.
struct ExpressionScene<'a> {
    clips: &'a [CachedClip],
    active: &'a [usize],
    current_frame: i32,
    layer_settings: &'a BTreeMap<i32, LayerSetting>,
}

impl ExpressionScene<'_> {
    fn relative_frame(&self, clip_index: usize) -> i32 {
        self.clips.get(clip_index).map_or(0, |clip| {
            self.current_frame.saturating_sub(clip.start_frame).max(0)
        })
    }

    fn resolve(
        &self,
        clip_index: usize,
        effect_index: usize,
        name: &str,
        visiting: &mut HashSet<(usize, usize, String)>,
        cache: &mut HashMap<(usize, usize, String), Option<f64>>,
    ) -> Option<f64> {
        let key = (clip_index, effect_index, name.to_owned());
        if let Some(cached) = cache.get(&key) {
            return *cached;
        }
        if !visiting.insert(key.clone()) {
            return None;
        }
        let resolved = (|| {
            let effect = self.clips.get(clip_index)?.effects.get(effect_index)?;
            let source = effect.expression_source(name)?;
            let frame = self.relative_frame(clip_index);
            let substituted = substitute_references(source, &mut |chain| {
                self.resolve_chain(clip_index, effect_index, name, chain, visiting, cache)
            });
            let value = effect.raw_numeric(name, frame).unwrap_or(0.0);
            expression::evaluate(&substituted, effect.time_seconds(frame), 0, value)
        })();
        visiting.remove(&key);
        cache.insert(key, resolved);
        resolved
    }

    /// Resolves one chain such as `X`, `$`, `ぼかし.範囲`, `above.X` or
    /// `layer1.ぼかし.範囲`.
    fn resolve_chain(
        &self,
        clip_index: usize,
        effect_index: usize,
        self_param: &str,
        chain: &str,
        visiting: &mut HashSet<(usize, usize, String)>,
        cache: &mut HashMap<(usize, usize, String), Option<f64>>,
    ) -> Option<f64> {
        if chain == "$" {
            return self.resolve_parameter(
                clip_index,
                effect_index,
                self_param,
                self_param,
                visiting,
                cache,
            );
        }
        let parts = chain.split('.').collect::<Vec<_>>();
        match parts.as_slice() {
            [param] => {
                self.resolve_parameter(clip_index, effect_index, self_param, param, visiting, cache)
            }
            [scope, param] => {
                if self.is_layer_scope(scope) {
                    let target = self.layer_target(clip_index, scope)?;
                    let target_effect = self.same_effect_on(target, clip_index, effect_index)?;
                    return self.resolve_parameter(
                        target,
                        target_effect,
                        self_param,
                        param,
                        visiting,
                        cache,
                    );
                }
                let target_effect = self.effect_by_name(clip_index, scope)?;
                self.resolve_parameter(
                    clip_index,
                    target_effect,
                    self_param,
                    param,
                    visiting,
                    cache,
                )
            }
            [scope, effect_name, param] => {
                let target = self.layer_target(clip_index, scope)?;
                let target_effect = self.effect_by_name(target, effect_name)?;
                self.resolve_parameter(target, target_effect, self_param, param, visiting, cache)
            }
            _ => None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_parameter(
        &self,
        clip_index: usize,
        effect_index: usize,
        self_param: &str,
        param: &str,
        visiting: &mut HashSet<(usize, usize, String)>,
        cache: &mut HashMap<(usize, usize, String), Option<f64>>,
    ) -> Option<f64> {
        let param = if param == "$" { self_param } else { param };
        let effect = self.clips.get(clip_index)?.effects.get(effect_index)?;
        if effect.expression_source(param).is_some() {
            return self.resolve(clip_index, effect_index, param, visiting, cache);
        }
        effect.raw_numeric(param, self.relative_frame(clip_index))
    }

    fn effect_by_name(&self, clip_index: usize, name: &str) -> Option<usize> {
        self.clips
            .get(clip_index)?
            .effects
            .iter()
            .position(|effect| effect.name == name || effect.id == name)
    }

    /// Finds the effect on another layer that corresponds to the referencing
    /// one: same display name when both carry one, otherwise the same id.
    fn same_effect_on(
        &self,
        target_clip: usize,
        source_clip: usize,
        source_effect: usize,
    ) -> Option<usize> {
        let source = self.clips.get(source_clip)?.effects.get(source_effect)?;
        self.clips
            .get(target_clip)?
            .effects
            .iter()
            .position(|effect| {
                (!source.name.is_empty() && effect.name == source.name) || effect.id == source.id
            })
    }

    /// Whether a scope token names a layer rather than an effect.
    fn is_layer_scope(&self, scope: &str) -> bool {
        let lower = scope.to_ascii_lowercase();
        lower.starts_with("layer")
            || lower.starts_with("above")
            || lower.starts_with("under")
            || self
                .layer_settings
                .values()
                .any(|setting| !setting.name.is_empty() && setting.name == scope)
    }

    fn layer_target(&self, from_clip: usize, scope: &str) -> Option<usize> {
        let from_layer = self.clips.get(from_clip)?.layer;
        let lower = scope.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("layer")
            && let Ok(layer) = rest.parse::<i32>()
        {
            return self.clip_on_layer(layer);
        }
        if let Some(layer) = self
            .layer_settings
            .iter()
            .find(|(_, setting)| !setting.name.is_empty() && setting.name == scope)
            .map(|(layer, _)| *layer)
        {
            return self.clip_on_layer(layer);
        }
        let (direction, digits) = if let Some(rest) = lower.strip_prefix("above") {
            (1, rest)
        } else {
            (-1, lower.strip_prefix("under")?)
        };
        let count = if digits.is_empty() {
            1
        } else {
            digits.parse::<usize>().ok().filter(|count| *count > 0)?
        };
        let layers = self.active_layers();
        let layer = if direction > 0 {
            layers
                .iter()
                .copied()
                .filter(|layer| *layer > from_layer)
                .nth(count - 1)
        } else {
            layers
                .iter()
                .rev()
                .copied()
                .filter(|layer| *layer < from_layer)
                .nth(count - 1)
        }?;
        self.clip_on_layer(layer)
    }

    /// Every layer that has an active clip at this frame, sorted ascending.
    fn active_layers(&self) -> Vec<i32> {
        let mut layers = self
            .active
            .iter()
            .filter_map(|&index| self.clips.get(index).map(|clip| clip.layer))
            .collect::<Vec<_>>();
        layers.sort_unstable();
        layers.dedup();
        layers
    }

    fn clip_on_layer(&self, layer: i32) -> Option<usize> {
        self.active.iter().copied().find(|&index| {
            self.clips
                .get(index)
                .is_some_and(|clip| clip.layer == layer)
        })
    }
}

/// Replaces every resolvable reference chain with its number, leaving unknown
/// chains untouched so the Lua parse fails and the literal value survives.
fn substitute_references(source: &str, resolve: &mut impl FnMut(&str) -> Option<f64>) -> String {
    let mut output = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(character) = rest.chars().next() {
        if character == '@' {
            let after = &rest[1..];
            let name_len = after
                .char_indices()
                .take_while(|(_, character)| character.is_alphanumeric() || *character == '_')
                .map(|(index, character)| index + character.len_utf8())
                .last()
                .unwrap_or(0);
            if name_len > 0 && after[name_len..].starts_with('(') {
                output.push_str(&after[..name_len]);
                rest = &after[name_len..];
                continue;
            }
            output.push(character);
            rest = after;
            continue;
        }
        if is_chain_start(character) {
            let chain_len = rest
                .char_indices()
                .take_while(|(_, character)| is_chain_character(*character))
                .map(|(index, character)| index + character.len_utf8())
                .last()
                .unwrap_or(0);
            let chain = &rest[..chain_len];
            match resolve(chain).and_then(format_resolved) {
                Some(number) => output.push_str(&number),
                None => output.push_str(chain),
            }
            rest = &rest[chain_len..];
            continue;
        }
        output.push(character);
        rest = &rest[character.len_utf8()..];
    }
    output
}

fn is_chain_start(character: char) -> bool {
    character.is_alphabetic() || character == '_' || character == '$'
}

fn is_chain_character(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '_' | '$' | '.')
}

fn format_resolved(value: f64) -> Option<String> {
    value.is_finite().then(|| format!("{value}"))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EvaluationKey {
    current_frame: i32,
}

struct BakePlan {
    fps: f64,
    scene_width: i32,
    scene_height: i32,
    scene_id: i32,
    clips: Vec<CachedClip>,
    scene_durations: HashMap<i32, i32>,
    layer_settings: BTreeMap<i32, LayerSetting>,
    time_buckets: HashMap<i32, Vec<usize>>,
    effect_count: usize,
}

/// A layer setting reduced to what the frame evaluator needs.
#[derive(Debug, Clone, PartialEq)]
struct LayerSetting {
    /// The user's layer name; references may address a layer by it.
    name: String,
    x: f32,
    y: f32,
    z: f32,
    /// Linear multiplier derived from the stored percentage.
    volume: f32,
}

impl LayerSetting {
    fn from_document(setting: &LayerSettingDocument) -> Self {
        // A non-finite value cannot come from a normalized document, but the
        // evaluator also runs against documents built in tests: an unusable
        // volume falls back to unity instead of muting the layer.
        let volume = if setting.volume_percent.is_finite() {
            (setting.volume_percent / 100.0) as f32
        } else {
            1.0
        };
        Self {
            name: setting.name.trim().to_owned(),
            x: finite_f32(setting.x),
            y: finite_f32(setting.y),
            z: finite_f32(setting.z),
            volume,
        }
    }
}

fn finite_f32(value: f64) -> f32 {
    if value.is_finite() { value as f32 } else { 0.0 }
}

impl BakePlan {
    #[cfg(test)]
    fn parse(input: &[u8]) -> Result<Self, ()> {
        let input: SceneInput = serde_json::from_slice(input).map_err(|_| ())?;
        if input.max_clip_id <= 0 {
            return Err(());
        }
        let fps = input.fps;
        let max_clip_id = input.max_clip_id;
        let clips = input
            .clips
            .into_iter()
            .filter(|clip| clip.id >= 0 && clip.id < max_clip_id)
            .map(|clip| CachedClip::new(clip, fps))
            .collect();
        let layer_settings = input
            .layer_settings
            .iter()
            .map(|setting| (setting.layer, LayerSetting::from_document(setting)))
            .collect();
        Ok(Self::from_clips(
            fps,
            input.width,
            input.height,
            0,
            clips,
            HashMap::new(),
            layer_settings,
        ))
    }

    fn from_document(
        document: &ProjectDocument,
        scene_id: i32,
    ) -> Result<Self, SceneRenderPlanError> {
        let scene = document
            .scenes
            .iter()
            .find(|scene| scene.id == scene_id)
            .ok_or(SceneRenderPlanError::SceneNotFound(scene_id))?;
        let clips = document
            .clips
            .iter()
            .filter(|clip| clip.scene_id == scene_id)
            .map(|clip| CachedClip::from_document(clip, scene.fps))
            .collect();
        let scene_durations = document
            .scenes
            .iter()
            .map(|scene| (scene.id, scene.duration))
            .collect();
        let layer_settings = scene
            .layer_settings
            .iter()
            .map(|setting| (setting.layer, LayerSetting::from_document(setting)))
            .collect();
        Ok(Self::from_clips(
            scene.fps,
            scene.width,
            scene.height,
            scene.id,
            clips,
            scene_durations,
            layer_settings,
        ))
    }

    fn from_clips(
        fps: f64,
        scene_width: i32,
        scene_height: i32,
        scene_id: i32,
        clips: Vec<CachedClip>,
        scene_durations: HashMap<i32, i32>,
        layer_settings: BTreeMap<i32, LayerSetting>,
    ) -> Self {
        let mut time_buckets: HashMap<i32, Vec<usize>> = HashMap::new();
        let effect_count = clips.iter().map(|clip| clip.effects.len()).sum();
        for (cached_index, cached) in clips.iter().enumerate() {
            let first_bucket = cached.start_frame / FRAME_BUCKET_SIZE;
            let last_bucket = cached.end_frame.max(cached.start_frame) / FRAME_BUCKET_SIZE;
            for bucket in first_bucket..=last_bucket {
                time_buckets.entry(bucket).or_default().push(cached_index);
            }
        }
        Self {
            fps,
            scene_width,
            scene_height,
            scene_id,
            clips,
            scene_durations,
            layer_settings,
            time_buckets,
            effect_count,
        }
    }

    fn selected_clips(&self, key: EvaluationKey) -> Vec<usize> {
        let range_start = key.current_frame;
        let range_end = key.current_frame;
        let first_bucket = range_start / FRAME_BUCKET_SIZE;
        let last_bucket = range_end / FRAME_BUCKET_SIZE;
        let mut seen = HashSet::new();
        let mut selected = Vec::new();
        for bucket in first_bucket..=last_bucket {
            let Some(indices) = self.time_buckets.get(&bucket) else {
                continue;
            };
            for &index in indices {
                let clip = &self.clips[index];
                if !seen.insert(clip.clip_id) {
                    continue;
                }
                if clip.start_frame <= range_end && clip.end_frame >= range_start {
                    selected.push(index);
                }
            }
        }
        selected
    }

    fn compute_frame(&mut self, key: EvaluationKey) -> SceneFramePlan {
        self.compute_output(key)
    }

    fn compute_output(&mut self, key: EvaluationKey) -> SceneFramePlan {
        let selected = self.selected_clips(key);
        let mut frame_layers = Vec::new();
        let mut frame_audio = Vec::new();

        for &index in &selected {
            let clip = &mut self.clips[index];
            let relative_frame = key.current_frame.saturating_sub(clip.start_frame).max(0);
            for effect in &mut clip.effects {
                effect.prepare_numeric(relative_frame);
            }
            for plugin in &mut clip.audio_plugins {
                plugin.prepare_numeric(relative_frame);
            }
        }
        // References can point at other layers, so they are resolved once every
        // clip has its own values for this frame.
        resolve_scene_expressions(
            &mut self.clips,
            &selected,
            key.current_frame,
            &self.layer_settings,
        );
        let mut group_controls = selected
            .iter()
            .filter_map(|&index| {
                let clip = &self.clips[index];
                (clip.start_frame <= key.current_frame && key.current_frame < clip.end_frame)
                    .then(|| evaluated_group_control(clip, key.current_frame))
                    .flatten()
            })
            .collect::<Vec<_>>();
        group_controls.sort_by_key(|control| control.layer);
        let camera = selected
            .iter()
            .filter_map(|&index| {
                let clip = &self.clips[index];
                (clip.start_frame <= key.current_frame && key.current_frame < clip.end_frame)
                    .then(|| evaluated_camera_control(clip, key.current_frame, self.scene_height))
                    .flatten()
                    .map(|camera| (clip.layer, camera))
            })
            .min_by_key(|(layer, _)| *layer)
            .map(|(_, camera)| camera);
        // AviUtl2's layer settings skip objects a camera control has taken over,
        // so the range each active camera covers is computed once per frame.
        let camera_targets = selected
            .iter()
            .filter_map(|&index| {
                let clip = &self.clips[index];
                if !(clip.start_frame <= key.current_frame && key.current_frame < clip.end_frame)
                    || !matches!(clip.clip_type.as_str(), "camera_control" | "camera")
                {
                    return None;
                }
                let relative_frame = key.current_frame.saturating_sub(clip.start_frame).max(0);
                let effect = clip.effects.iter().find(|effect| {
                    effect.enabled && matches!(effect.id.as_str(), "camera_control" | "camera")
                })?;
                let layer_count = effect
                    .float_or("layerCount", 1.0, relative_frame)
                    .round()
                    .max(1.0) as i32;
                Some((clip.layer, layer_count))
            })
            .min_by_key(|(layer, _)| *layer);

        for index in selected {
            let clip = &mut self.clips[index];
            let relative_frame = key.current_frame.saturating_sub(clip.start_frame).max(0);

            if clip.clip_type == "audio" || clip.clip_type == "video" {
                let mut input = AudioBakeInput {
                    clip_id: clip.clip_id,
                    start_frame: clip.start_frame,
                    duration_frames: clip.duration_frames,
                    has_audio_effect: 0,
                    fps: self.fps,
                    source_start_time: 0.0,
                    speed_percent: DEFAULT_SPEED_PERCENT,
                    direct_time: 0.0,
                    volume: 1.0,
                    master_volume: 1.0,
                    pan: 0.0,
                    fade_in_seconds: 0.0,
                    fade_out_seconds: 0.0,
                    direct_mode: 0,
                    mute: 0,
                    solo: 0,
                    limiter: 0,
                };
                if matches!(self.fps.partial_cmp(&0.0), Some(Ordering::Greater) | None)
                    && let Some(effect) = clip
                        .effects
                        .iter()
                        .find(|effect| effect.enabled && effect.id == "audio")
                {
                    input.has_audio_effect = 1;
                    input.direct_mode = u32::from(matches!(
                        effect
                            .params
                            .get("playMode")
                            .map(payload)
                            .and_then(Value::as_str)
                            .and_then(playback_mode),
                        Some(PlaybackMode::Direct)
                    ));
                    input.source_start_time = effect.float_or("startTime", 0.0, relative_frame);
                    input.speed_percent =
                        effect.float_or("speed", DEFAULT_SPEED_PERCENT, relative_frame);
                    input.direct_time = effect.float_or("directTime", 0.0, relative_frame);
                    input.volume = effect.float_or("volume", 1.0, relative_frame);
                    input.master_volume = effect.float_or("masterVolume", 1.0, relative_frame);
                    input.pan = effect.float_or("pan", 0.0, relative_frame);
                    input.fade_in_seconds = effect.float_or("fadeIn", 0.0, relative_frame);
                    input.fade_out_seconds = effect.float_or("fadeOut", 0.0, relative_frame);
                    input.mute = u32::from(effect.flag_or("mute", false, relative_frame));
                    input.solo = u32::from(effect.flag_or("solo", false, relative_frame));
                    input.limiter = u32::from(effect.flag_or("limiter", true, relative_frame));
                }
                let mut output = bake_audio(input);
                // Scene projection needs a stable entity key even when the legacy single-clip
                // kernel returns its invalid-fps sentinel payload.
                output.clip_id = clip.clip_id;
                if !camera_covers(camera_targets, clip.layer)
                    && let Some(setting) = self.layer_settings.get(&clip.layer)
                {
                    // The layer volume multiplies the object volume; keep the
                    // result non-negative even when a hand-built document has a
                    // value the normalizer would have clamped.
                    output.volume = (output.volume * setting.volume).max(0.0);
                }
                if clip.start_frame <= key.current_frame && key.current_frame < clip.end_frame {
                    frame_audio.push(audio_layer_plan(
                        output,
                        audio_source_path(&clip.clip_type, &clip.effects, relative_frame),
                        evaluated_audio_plugins(&clip.audio_plugins, relative_frame),
                    ));
                }
            }

            let mut render_input = RenderBakeInput {
                has_transform: 0,
                x: 0.0,
                y: 0.0,
                z: 0.0,
                rotation_x: 0.0,
                rotation_y: 0.0,
                rotation_z: 0.0,
                scale: 100.0,
                opacity: 1.0,
                aspect: 0.0,
                pivot_x: 0.0,
                pivot_y: 0.0,
                pivot_z: 0.0,
            };
            let mut evaluated_effects = Vec::new();

            for (effect_index, effect) in clip.effects.iter().enumerate() {
                if !effect.enabled || !effect.known {
                    continue;
                }
                if effect.id == "transform" {
                    // A transform that omits a parameter must keep AviUtl's value for
                    // it, not fall to zero: an object saved with only `x` would
                    // otherwise draw at scale 0 - invisible - in the preview and in an
                    // export, while every parameter it does carry looks correct.
                    render_input.has_transform = 1;
                    render_input.x = effect.float_or("x", 0.0, relative_frame);
                    render_input.y = effect.float_or("y", 0.0, relative_frame);
                    render_input.z = effect.float_or("z", 0.0, relative_frame);
                    render_input.rotation_x = effect.float_or("rotationX", 0.0, relative_frame);
                    render_input.rotation_y = effect.float_or("rotationY", 0.0, relative_frame);
                    render_input.rotation_z = effect.float_or("rotationZ", 0.0, relative_frame);
                    render_input.scale = effect.float_or("scale", 100.0, relative_frame);
                    render_input.opacity = effect.float_or("opacity", 1.0, relative_frame);
                    render_input.aspect = effect.float_or("aspect", 0.0, relative_frame);
                    render_input.pivot_x = effect.float_or("cx", 0.0, relative_frame);
                    render_input.pivot_y = effect.float_or("cy", 0.0, relative_frame);
                    render_input.pivot_z = effect.float_or("cz", 0.0, relative_frame);
                }
                let mut evaluated_params = BTreeMap::new();
                for name in &effect.all_keys {
                    evaluated_params.insert(name.clone(), effect.value(name, relative_frame));
                }
                evaluated_effects.push(EvaluatedEffect {
                    effect_index,
                    id: effect.id.clone(),
                    params: evaluated_params,
                });
            }
            let render = bake_render(render_input);
            if clip.start_frame <= key.current_frame && key.current_frame < clip.end_frame {
                let mut transform = EvaluatedTransform {
                    x: render.x,
                    y: render.y,
                    z: render.z,
                    rotation_x: render.rotation_x,
                    rotation_y: render.rotation_y,
                    rotation_z: render.rotation_z,
                    scale_percent: render.scale_x * 100.0,
                    opacity: render.opacity,
                    aspect: render_input.aspect,
                    pivot_x: render_input.pivot_x,
                    pivot_y: render_input.pivot_y,
                    pivot_z: render_input.pivot_z,
                };
                let media = media_playback_plan(
                    &clip.clip_type,
                    relative_frame,
                    self.fps,
                    &evaluated_effects,
                );
                let shape = shape_render_plan(&clip.clip_type, &evaluated_effects);
                let text = text_render_plan(
                    &clip.clip_type,
                    relative_frame,
                    clip.duration_frames,
                    self.fps,
                    &evaluated_effects,
                    &TextObjectContext {
                        clip_id: clip.clip_id,
                        layer: clip.layer,
                        start_frame: clip.start_frame,
                        end_frame: clip.end_frame,
                        screen_width: self.scene_width,
                        screen_height: self.scene_height,
                        scene_id: self.scene_id,
                    },
                    &mut transform,
                );
                // A text script sees and writes the object's own transform, so
                // the group control and layer nudge are applied on top of what
                // the script left rather than being folded into its inputs.
                apply_group_controls(&mut transform, clip.layer, &group_controls);
                if !camera_covers(camera_targets, clip.layer)
                    && !is_control_object(&clip.clip_type)
                    && let Some(setting) = self.layer_settings.get(&clip.layer)
                {
                    // Layer offsets are a screen-space nudge added after the
                    // object's own transform and any group control around it.
                    transform.x += setting.x;
                    transform.y += setting.y;
                    transform.z += setting.z;
                }
                let procedural = procedural_object_render_plan(
                    &clip.clip_type,
                    relative_frame,
                    &evaluated_effects,
                );
                let nested_scene = nested_scene_render_plan(
                    &clip.clip_type,
                    relative_frame,
                    &evaluated_effects,
                    &self.scene_durations,
                );
                frame_layers.push(RenderLayerPlan {
                    clip_id: clip.clip_id,
                    clip_type: clip.clip_type.clone(),
                    timeline_layer: clip.layer,
                    start_frame: clip.start_frame,
                    duration_frames: clip.duration_frames,
                    relative_frame,
                    clip_by_upper_object: clip.clip_by_upper_object,
                    transform,
                    effects: evaluated_effects,
                    media,
                    shape,
                    text,
                    procedural,
                    nested_scene,
                });
            }
        }

        SceneFramePlan {
            frame: key.current_frame,
            layers: frame_layers,
            audio: frame_audio,
            camera,
        }
    }
}

fn value_as_f64(value: Option<&Value>, fallback: f64) -> f64 {
    value
        .map(payload)
        .and_then(|value| {
            numeric_value(value).or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        })
        .filter(|value| value.is_finite())
        .unwrap_or(fallback)
}

fn value_as_bool(value: Option<&Value>, fallback: bool) -> bool {
    value
        .map(payload)
        .and_then(|value| {
            value.as_bool().or_else(|| {
                value
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .map(|value| value != 0.0)
            })
        })
        .unwrap_or(fallback)
}

fn value_as_string(value: Option<&Value>, fallback: &str) -> String {
    value
        .map(payload)
        .and_then(Value::as_str)
        .unwrap_or(fallback)
        .to_owned()
}

fn value_as_color(value: Option<&Value>, fallback: &str) -> RgbaColor {
    let [alpha, red, green, blue] = value
        .map(payload)
        .and_then(Value::as_str)
        .and_then(parse_hex_color)
        .or_else(|| parse_hex_color(fallback))
        .expect("built-in shape colors are valid");
    RgbaColor {
        red,
        green,
        blue,
        alpha,
    }
}

fn shape_render_plan(clip_type: &str, effects: &[EvaluatedEffect]) -> Option<ShapeRenderPlan> {
    let effect = effects.iter().find(|effect| effect.id == clip_type)?;
    let value = |name| effect.params.get(name);
    let (
        kind,
        width_default,
        height_default,
        sides,
        sweep_degrees,
        normalize_vertices,
        even_sides_half_step,
        edge_padding,
    ) = match clip_type {
        "rect" => {
            let kind = match value("shapeType")
                .map(payload)
                .and_then(Value::as_str)
                .unwrap_or("polygon")
            {
                "star" => ShapeKind::Star,
                "pie" => ShapeKind::Pie,
                "arc" => ShapeKind::Arc,
                "donut" => ShapeKind::Donut,
                _ => ShapeKind::Polygon,
            };
            let sides = value_as_f64(value("sides"), 4.0).round().clamp(3.0, 360.0) as u32;
            (kind, 200.0, 200.0, sides, sides as f32, true, true, 2.0)
        }
        "polygon_shape" => (
            ShapeKind::Polygon,
            1_920.0,
            1_080.0,
            value_as_f64(value("sides"), 6.0).round().clamp(3.0, 64.0) as u32,
            360.0,
            false,
            false,
            4.0,
        ),
        "pie_shape" => (
            ShapeKind::Pie,
            1_920.0,
            1_080.0,
            3,
            value_as_f64(value("angle"), 180.0).clamp(1.0, 360.0) as f32,
            false,
            false,
            4.0,
        ),
        _ => return None,
    };
    Some(ShapeRenderPlan {
        kind,
        width: value_as_f64(value("sizeW"), width_default).max(0.0) as f32,
        height: value_as_f64(value("sizeH"), height_default).max(0.0) as f32,
        sides,
        sweep_degrees,
        corner_radius: value_as_f64(value("cornerRadius"), 0.0).max(0.0) as f32,
        inner_radius_percent: value_as_f64(value("innerRadius"), 50.0).clamp(0.0, 100.0) as f32,
        rotation_degrees: value_as_f64(value("rotation"), 0.0) as f32,
        normalize_vertices,
        even_sides_half_step,
        edge_padding,
        fill_color: value_as_color(value("color"), "#66aa99"),
        use_gradient: value_as_bool(value("useGradient"), false),
        gradient_color: value_as_color(value("gradientColor2"), "#ffffff"),
        gradient_kind: if value_as_f64(value("gradientType"), 0.0).round() == 1.0 {
            ShapeGradientKind::Radial
        } else {
            ShapeGradientKind::Linear
        },
        stroke_color: value_as_color(value("strokeColor"), "#ffffff"),
        stroke_width: value_as_f64(value("strokeWidth"), 0.0).max(0.0) as f32,
        dash_length: value_as_f64(value("dashLength"), 0.0).max(0.0) as f32,
        dash_space: value_as_f64(value("dashSpace"), 0.0).max(0.0) as f32,
        opacity: value_as_f64(value("opacity"), 1.0).clamp(0.0, 1.0) as f32,
    })
}

/// The scene and object facts a text script reads through `obj`.
struct TextObjectContext {
    clip_id: i32,
    layer: i32,
    start_frame: i32,
    end_frame: i32,
    screen_width: i32,
    screen_height: i32,
    scene_id: i32,
}

fn text_render_plan(
    clip_type: &str,
    relative_frame: i32,
    duration_frames: i32,
    fps: f64,
    effects: &[EvaluatedEffect],
    context: &TextObjectContext,
    // A text script may write `obj.rz` and friends, which changes this frame's
    // transform; the caller applies it after this returns.
    transform: &mut EvaluatedTransform,
) -> Option<TextRenderPlan> {
    let effect = effects.iter().find(|effect| effect.id == clip_type)?;
    let value = |name| effect.params.get(name);
    if clip_type == "counter" {
        let outline_width = value_as_f64(value("outlineWidth"), 2.0).clamp(0.0, 512.0) as f32;
        let content = counter_text(&effect.params, relative_frame, duration_frames, fps);
        let font_family = value_as_string(value("fontFamily"), "sans-serif");
        let font_size = value_as_f64(value("fontSize"), 64.0).clamp(1.0, 4_096.0) as f32;
        let color = value_as_color(value("color"), "#ffffff");
        let outline_color = value_as_color(value("outlineColor"), "#000000");
        return Some(TextRenderPlan {
            runs_authoritative: true,
            runs: vec![TextRun {
                text: content.clone(),
                color,
                outline_color,
                font_size,
                letter_spacing: 0.0,
                line_spacing: None,
                anchor: crate::text_control::TextAnchor::Flow,
                x_scale: 1.0,
                y_scale: 1.0,
                rotation: 0.0,
                ruby: None,
                emoji: None,
                font_family: None,
                outline_width: None,
                bold: false,
                italic: false,
                strikethrough: false,
            }],
            content,
            font_family,
            font_size,
            bold: false,
            italic: false,
            letter_spacing: 0.0,
            line_spacing: 0.0,
            alignment: TextAlignment::Center,
            color,
            outline_enabled: outline_width > 0.0,
            outline_color,
            outline_width,
            shadow_enabled: false,
            shadow_color: value_as_color(None, "#00000000"),
            shadow_offset_x: 0.0,
            shadow_offset_y: 0.0,
            background_enabled: false,
            background_color: value_as_color(None, "#00000000"),
            background_radius: 0.0,
            background_padding_x: 0.0,
            background_padding_y: 0.0,
            opacity: value_as_f64(value("opacity"), 1.0).clamp(0.0, 1.0) as f32,
        });
    }
    if clip_type != "text" {
        return None;
    }
    let alignment = match value_as_f64(value("alignment"), 4.0).round() as i32 {
        1 => TextAlignment::Left,
        2 => TextAlignment::Right,
        8 => TextAlignment::Justify,
        _ => TextAlignment::Center,
    };
    let raw_content = value_as_string(value("text"), "テキスト");
    // Scripts run before the control-character parse, so their output takes
    // part in timing, colours and the rest of the text pipeline.
    let seconds = relative_frame as f64 / fps.max(0.001);
    let (content, scripted) = crate::expression::expand_text_scripts(
        &raw_content,
        &crate::expression::TextScriptClock {
            time: seconds,
            total_time: duration_frames as f64 / fps.max(0.001),
            frame: relative_frame,
            total_frames: duration_frames,
            frame_rate: fps,
            layer: context.layer,
            start_frame: context.start_frame,
            end_frame: context.end_frame,
            screen_width: context.screen_width,
            screen_height: context.screen_height,
            scene_id: context.scene_id,
            clip_id: context.clip_id,
            transform: crate::expression::TextScriptTransform {
                x: transform.x,
                y: transform.y,
                z: transform.z,
                rotation_x: transform.rotation_x,
                rotation_y: transform.rotation_y,
                rotation_z: transform.rotation_z,
                scale_percent: transform.scale_percent,
                opacity: transform.opacity,
                aspect: transform.aspect,
                pivot_x: transform.pivot_x,
                pivot_y: transform.pivot_y,
                pivot_z: transform.pivot_z,
            },
        },
    );
    // A script that changed the object's transform wins for this frame, the
    // same way `<?obj.rz = obj.time * 360?>` rotates the object in AviUtl.
    if let Some(scripted) = scripted {
        transform.x = scripted.x;
        transform.y = scripted.y;
        transform.z = scripted.z;
        transform.rotation_x = scripted.rotation_x;
        transform.rotation_y = scripted.rotation_y;
        transform.rotation_z = scripted.rotation_z;
        transform.scale_percent = scripted.scale_percent;
        transform.opacity = scripted.opacity;
        transform.aspect = scripted.aspect;
        transform.pivot_x = scripted.pivot_x;
        transform.pivot_y = scripted.pivot_y;
        transform.pivot_z = scripted.pivot_z;
    }
    let font_family = value_as_string(value("fontFamily"), "sans-serif");
    let font_size = value_as_f64(value("fontSize"), 48.0).clamp(1.0, 4_096.0) as f32;
    let letter_spacing = value_as_f64(value("letterSpacing"), 0.0) as f32;
    let color = value_as_color(value("color"), "#ffffff");
    let outline_color = value_as_color(value("outlineColor"), "#000000");
    let speed = value_as_f64(value("speed"), 0.0).clamp(0.0, 1_000.0) as f32;
    let time = seconds as f32;
    let line_spacing = value_as_f64(value("lineSpacing"), 0.0).max(0.0) as f32;
    let bold = value_as_bool(value("fontBold"), false);
    let italic = value_as_bool(value("fontItalic"), false);
    let runs = crate::text_control::parse_text_runs_at(
        &content,
        TextBaseStyle {
            color,
            outline_color,
            font_size,
            letter_spacing,
            line_spacing: (line_spacing > 0.0).then_some(line_spacing),
            anchor: crate::text_control::TextAnchor::Flow,
            x_scale: 1.0,
            y_scale: 1.0,
            rotation: 0.0,
            ruby: None,
            emoji: None,
            font_family: None,
            // The object's own outline width is the fallback a bare `<s>`
            // restores; a run without `<s...,outline>` keeps it.
            outline_width: None,
            bold,
            italic,
        },
        speed,
        time,
    );
    Some(TextRenderPlan {
        runs_authoritative: true,
        runs,
        content,
        font_family,
        font_size,
        bold,
        italic,
        letter_spacing,
        line_spacing,
        alignment,
        color,
        outline_enabled: value_as_bool(value("outlineEnabled"), false),
        outline_color,
        outline_width: value_as_f64(value("outlineWidth"), 2.0).clamp(0.0, 512.0) as f32,
        shadow_enabled: value_as_bool(value("shadowEnabled"), false),
        shadow_color: value_as_color(value("shadowColor"), "#80000000"),
        shadow_offset_x: value_as_f64(value("shadowOffsetX"), 5.0) as f32,
        shadow_offset_y: value_as_f64(value("shadowOffsetY"), 5.0) as f32,
        background_enabled: value_as_bool(value("bgEnabled"), false),
        background_color: value_as_color(value("backgroundColor"), "#80000000"),
        background_radius: value_as_f64(value("backgroundRadius"), 10.0).max(0.0) as f32,
        background_padding_x: value_as_f64(value("backgroundPaddingX"), 20.0).max(0.0) as f32,
        background_padding_y: value_as_f64(value("backgroundPaddingY"), 10.0).max(0.0) as f32,
        opacity: 1.0,
    })
}

fn counter_text(
    params: &BTreeMap<String, Value>,
    relative_frame: i32,
    duration_frames: i32,
    fps: f64,
) -> String {
    let value = |name| params.get(name);
    let mode = value_as_string(value("mode"), "frame");
    let counter_value = match mode.as_str() {
        "time" => f64::from(relative_frame) / fps.max(0.001),
        "value" => {
            let progress = if duration_frames > 0 {
                (f64::from(relative_frame) / f64::from(duration_frames)).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let start = value_as_f64(value("startValue"), 0.0);
            let end = value_as_f64(value("endValue"), 100.0);
            start + (end - start) * progress
        }
        _ => f64::from(relative_frame),
    };
    let decimals = value_as_f64(value("decimals"), 0.0)
        .round()
        .clamp(0.0, 100.0) as usize;
    let digits = value_as_f64(value("digits"), 0.0)
        .round()
        .clamp(0.0, 4_096.0) as usize;
    let mut number = format!("{counter_value:.decimals$}");
    let negative = number.starts_with('-');
    let body = number.strip_prefix('-').unwrap_or(&number);
    let (integer, fraction) = body.split_once('.').unwrap_or((body, ""));
    if digits > integer.len() {
        let mut padded = String::with_capacity(number.len() + digits - integer.len());
        if negative {
            padded.push('-');
        }
        padded.extend(std::iter::repeat_n('0', digits - integer.len()));
        padded.push_str(integer);
        if decimals > 0 {
            padded.push('.');
            padded.push_str(fraction);
        }
        number = padded;
    }
    format!(
        "{}{}{}",
        value_as_string(value("prefix"), ""),
        number,
        value_as_string(value("suffix"), "")
    )
}

fn procedural_object_render_plan(
    clip_type: &str,
    relative_frame: i32,
    effects: &[EvaluatedEffect],
) -> Option<ProceduralObjectRenderPlan> {
    let effect = effects.iter().find(|effect| effect.id == clip_type)?;
    let value = |name| effect.params.get(name);
    let width = value_as_f64(value("sizeW"), 1_920.0).max(1.0) as f32;
    let height = value_as_f64(value("sizeH"), 1_080.0).max(1.0) as f32;
    let opacity = value_as_f64(value("opacity"), 1.0).clamp(0.0, 1.0) as f32;
    match clip_type {
        "track_line" => Some(ProceduralObjectRenderPlan::TrackLine(TrackLineRenderPlan {
            width,
            height,
            start_x: value_as_f64(value("startX"), -240.0) as f32,
            start_y: value_as_f64(value("startY"), 120.0) as f32,
            end_x: value_as_f64(value("endX"), 240.0) as f32,
            end_y: value_as_f64(value("endY"), -120.0) as f32,
            line_width: value_as_f64(value("lineWidth"), 8.0) as f32,
            dash_length: value_as_f64(value("dashLength"), 0.0) as f32,
            dash_space: value_as_f64(value("dashSpace"), 12.0) as f32,
            arrow: value_as_bool(value("arrow"), true),
            color: value_as_color(value("color"), "#ffffff"),
            opacity,
        })),
        "star" => Some(ProceduralObjectRenderPlan::ParticleField(
            ParticleFieldRenderPlan {
                width,
                height,
                count: value_as_f64(value("count"), 120.0)
                    .round()
                    .clamp(1.0, 2_000.0) as u32,
                speed: value_as_f64(value("speed"), 1.0) as f32,
                particle_size: value_as_f64(value("particleSize"), 4.0) as f32,
                spread: value_as_f64(value("spread"), 1.0) as f32,
                seed: value_as_f64(value("seed"), 1.0).round() as i32,
                color: value_as_color(value("color"), "#ffffff"),
                opacity,
                relative_frame,
            },
        )),
        "radial_lines" => Some(ProceduralObjectRenderPlan::RadialLines(
            RadialLinesRenderPlan {
                width,
                height,
                line_count: value_as_f64(value("lineCount"), 128.0)
                    .round()
                    .clamp(1.0, 512.0) as u32,
                min_length: value_as_f64(value("minLength"), 760.0) as f32,
                max_length: value_as_f64(value("maxLength"), 1_500.0) as f32,
                thickness: value_as_f64(value("thickness"), 5.0) as f32,
                randomness: value_as_f64(value("randomness"), 0.75) as f32,
                center_x: value_as_f64(value("centerX"), 0.0) as f32,
                center_y: value_as_f64(value("centerY"), 0.0) as f32,
                spin_speed: value_as_f64(value("spinSpeed"), 0.0) as f32,
                seed: value_as_f64(value("seed"), 1.0).round() as i32,
                color: value_as_color(value("color"), "#ffffff"),
                opacity,
                relative_frame,
            },
        )),
        "flare" | "lens_flare_object" => {
            let (default_radius, default_strength, default_ghosts, default_color) =
                if clip_type == "flare" {
                    (90.0, 1.3, 0.0, "#ffd1a3")
                } else {
                    (180.0, 1.0, 4.0, "#fff2aa")
                };
            Some(ProceduralObjectRenderPlan::LensFlare(LensFlareRenderPlan {
                width,
                height,
                center_x: value_as_f64(value("centerX"), 0.0) as f32,
                center_y: value_as_f64(value("centerY"), 0.0) as f32,
                radius: value_as_f64(value("radius"), default_radius).max(0.0) as f32,
                strength: value_as_f64(value("strength"), default_strength).max(0.0) as f32,
                ghosts: value_as_f64(value("ghosts"), default_ghosts)
                    .round()
                    .clamp(0.0, 16.0) as u32,
                color: value_as_color(value("color"), default_color),
                opacity,
            }))
        }
        _ => None,
    }
}

fn evaluated_group_control(clip: &CachedClip, current_frame: i32) -> Option<EvaluatedGroupControl> {
    if clip.clip_type != "GroupControl" {
        return None;
    }
    let relative_frame = current_frame.saturating_sub(clip.start_frame).max(0);
    let effect = clip
        .effects
        .iter()
        .find(|effect| effect.enabled && effect.id == "GroupControl")?;
    Some(EvaluatedGroupControl {
        layer: clip.layer,
        layer_count: effect
            .float_or("layerCount", 1.0, relative_frame)
            .round()
            .max(1.0) as i32,
        x: f64::from(effect.float_or("x", 0.0, relative_frame)),
        y: f64::from(effect.float_or("y", 0.0, relative_frame)),
        z: f64::from(effect.float_or("z", 0.0, relative_frame)),
        scale: f64::from(effect.float_or("scale", 100.0, relative_frame)) / 100.0,
        rotation_x: f64::from(effect.float_or("rotationX", 0.0, relative_frame)),
        rotation_y: f64::from(effect.float_or("rotationY", 0.0, relative_frame)),
        rotation_z: f64::from(effect.float_or("rotationZ", 0.0, relative_frame)),
        opacity: effect.float_or("opacity", 1.0, relative_frame),
    })
}

fn evaluated_camera_control(
    clip: &CachedClip,
    current_frame: i32,
    scene_height: i32,
) -> Option<CameraRenderPlan> {
    if !matches!(clip.clip_type.as_str(), "camera_control" | "camera") {
        return None;
    }
    let relative_frame = current_frame.saturating_sub(clip.start_frame).max(0);
    let current = clip
        .effects
        .iter()
        .find(|effect| effect.enabled && effect.id == "camera_control");
    let legacy = clip
        .effects
        .iter()
        .find(|effect| effect.enabled && effect.id == "camera");
    current.or(legacy)?;
    let parameter = |name: &str, fallback: f32| {
        current
            .filter(|effect| effect.params.contains_key(name) || effect.tracks.contains_key(name))
            .or_else(|| {
                legacy.filter(|effect| {
                    effect.params.contains_key(name) || effect.tracks.contains_key(name)
                })
            })
            .map_or(fallback, |effect| effect.float(name, relative_frame))
    };
    let field_of_view_degrees = parameter("fov", 30.0).clamp(1.0, 170.0);
    let default_distance =
        scene_height.max(1) as f32 / (2.0 * (field_of_view_degrees.to_radians() / 2.0).tan());
    Some(CameraRenderPlan {
        position_x: parameter("x", 0.0),
        position_y: -parameter("y", 0.0),
        position_z: default_distance + parameter("z", 0.0),
        target_x: parameter("tx", 0.0),
        target_y: -parameter("ty", 0.0),
        target_z: parameter("tz", 0.0),
        roll_degrees: parameter("roll", 0.0),
        field_of_view_degrees,
    })
}

/// Whether a camera control's target range contains a layer.
fn camera_covers(camera: Option<(i32, i32)>, layer: i32) -> bool {
    camera.is_some_and(|(camera_layer, layer_count)| {
        layer > camera_layer && layer <= camera_layer.saturating_add(layer_count)
    })
}

/// Control objects keep their own coordinates; AviUtl2's layer settings only
/// move media objects.
fn is_control_object(clip_type: &str) -> bool {
    matches!(
        clip_type,
        "camera" | "camera_control" | "GroupControl" | "frame_buffer"
    )
}

fn apply_group_controls(
    transform: &mut EvaluatedTransform,
    layer: i32,
    controls: &[EvaluatedGroupControl],
) {
    let active = controls
        .iter()
        .filter(|control| {
            control.layer < layer && layer <= control.layer.saturating_add(control.layer_count)
        })
        .collect::<Vec<_>>();
    if active.is_empty() {
        return;
    }
    let mut matrix = identity_matrix();
    for control in active {
        matrix = multiply_matrix(matrix, translation_matrix(control.x, control.y, control.z));
        matrix = multiply_matrix(matrix, rotation_x_matrix(control.rotation_x.to_radians()));
        matrix = multiply_matrix(matrix, rotation_y_matrix(control.rotation_y.to_radians()));
        matrix = multiply_matrix(matrix, rotation_z_matrix(control.rotation_z.to_radians()));
        matrix = multiply_matrix(
            matrix,
            scale_matrix(control.scale, control.scale, control.scale),
        );
        transform.rotation_x += control.rotation_x as f32;
        transform.rotation_y += control.rotation_y as f32;
        transform.rotation_z += control.rotation_z as f32;
        transform.opacity *= control.opacity;
    }
    matrix = multiply_matrix(
        matrix,
        translation_matrix(
            f64::from(transform.x),
            f64::from(transform.y),
            f64::from(transform.z),
        ),
    );
    transform.x = matrix[12] as f32;
    transform.y = matrix[13] as f32;
    transform.z = matrix[14] as f32;
}

fn identity_matrix() -> [f64; 16] {
    [
        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]
}

fn multiply_matrix(left: [f64; 16], right: [f64; 16]) -> [f64; 16] {
    let mut result = [0.0; 16];
    for column in 0..4 {
        for row in 0..4 {
            result[column * 4 + row] = (0..4)
                .map(|index| left[index * 4 + row] * right[column * 4 + index])
                .sum();
        }
    }
    result
}

fn translation_matrix(x: f64, y: f64, z: f64) -> [f64; 16] {
    let mut matrix = identity_matrix();
    matrix[12] = x;
    matrix[13] = y;
    matrix[14] = z;
    matrix
}

fn scale_matrix(x: f64, y: f64, z: f64) -> [f64; 16] {
    [
        x, 0.0, 0.0, 0.0, 0.0, y, 0.0, 0.0, 0.0, 0.0, z, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]
}

fn rotation_x_matrix(radians: f64) -> [f64; 16] {
    let (sine, cosine) = radians.sin_cos();
    [
        1.0, 0.0, 0.0, 0.0, 0.0, cosine, sine, 0.0, 0.0, -sine, cosine, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]
}

fn rotation_y_matrix(radians: f64) -> [f64; 16] {
    let (sine, cosine) = radians.sin_cos();
    [
        cosine, 0.0, -sine, 0.0, 0.0, 1.0, 0.0, 0.0, sine, 0.0, cosine, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]
}

fn rotation_z_matrix(radians: f64) -> [f64; 16] {
    let (sine, cosine) = radians.sin_cos();
    [
        cosine, sine, 0.0, 0.0, -sine, cosine, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]
}

fn nested_scene_render_plan(
    clip_type: &str,
    relative_frame: i32,
    effects: &[EvaluatedEffect],
    scene_durations: &HashMap<i32, i32>,
) -> Option<NestedSceneRenderPlan> {
    if clip_type != "scene" {
        return None;
    }
    let effect = effects.iter().find(|effect| effect.id == "scene")?;
    let value = |name| effect.params.get(name);
    let target_scene_id = value_as_f64(value("targetSceneId"), -1.0).round() as i32;
    let duration = *scene_durations.get(&target_scene_id)?;
    let speed = value_as_f64(value("speed"), 1.0);
    let offset = value_as_f64(value("offset"), 0.0).floor();
    let target_frame = (f64::from(relative_frame) * speed).floor() + offset;
    Some(NestedSceneRenderPlan {
        target_scene_id,
        target_frame: target_frame.clamp(0.0, f64::from(duration.saturating_sub(1).max(0))) as i32,
        opacity: value_as_f64(value("opacity"), 1.0).clamp(0.0, 1.0) as f32,
    })
}

fn media_playback_plan(
    clip_type: &str,
    relative_frame: i32,
    fallback_fps: f64,
    effects: &[EvaluatedEffect],
) -> Option<MediaPlaybackPlan> {
    let kind = match clip_type {
        "image" => MediaKind::Image,
        "video" => MediaKind::Video,
        _ => return None,
    };
    let effect = effects.iter().find(|effect| effect.id == clip_type)?;
    let path = effect
        .params
        .get("path")
        .map(payload)
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())?
        .to_owned();
    let direct_mode = effect
        .params
        .get("playMode")
        .map(payload)
        .and_then(Value::as_str)
        .and_then(playback_mode)
        == Some(PlaybackMode::Direct);
    Some(MediaPlaybackPlan {
        kind,
        path,
        relative_frame,
        direct_mode,
        start_frame: value_as_f64(effect.params.get("startFrame"), 0.0),
        direct_frame: value_as_f64(effect.params.get("directFrame"), 0.0),
        speed_percent: value_as_f64(effect.params.get("speed"), 100.0),
        opacity: value_as_f64(effect.params.get("opacity"), 1.0) as f32,
        fallback_fps,
    })
}

fn audio_source_path(
    clip_type: &str,
    effects: &[CachedEffect],
    relative_frame: i32,
) -> Option<String> {
    let parameter = match clip_type {
        "audio" => "source",
        "video" => "path",
        _ => return None,
    };
    effects
        .iter()
        .find(|effect| effect.enabled && effect.id == clip_type)
        .map(|effect| effect.value(parameter, relative_frame))
        .as_ref()
        .map(payload)
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
}

fn evaluated_audio_plugins(
    plugins: &[CachedEffect],
    relative_frame: i32,
) -> Vec<EvaluatedAudioPlugin> {
    plugins
        .iter()
        .map(|plugin| EvaluatedAudioPlugin {
            id: plugin.id.clone(),
            enabled: plugin.enabled,
            params: plugin
                .all_keys
                .iter()
                .map(|name| (name.clone(), plugin.value(name, relative_frame)))
                .collect(),
            extra: plugin.extra.clone(),
        })
        .collect()
}

fn audio_layer_plan(
    output: AudioBakeOutput,
    source_path: Option<String>,
    plugins: Vec<EvaluatedAudioPlugin>,
) -> AudioLayerPlan {
    AudioLayerPlan {
        clip_id: output.clip_id,
        source_path,
        start_frame: output.start_frame,
        duration_frames: output.duration_frames,
        source_start_time: output.source_start_time,
        playback_speed: output.playback_speed,
        direct_time: output.direct_time,
        volume: output.volume,
        master_volume: output.master_volume,
        pan: output.pan,
        fade_in_seconds: output.fade_in_seconds,
        fade_out_seconds: output.fade_out_seconds,
        mute: output.mute != 0,
        solo: output.solo != 0,
        limiter: output.limiter != 0,
        direct_mode: output.direct_mode != 0,
        plugins,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scene_json() -> Vec<u8> {
        serde_json::to_vec(&json!({
            "fps": 60.0,
            "maxClipId": 4096,
            "clips": [{
                "id": 7,
                "layer": 3,
                "startFrame": 10,
                "durationFrames": 100,
                "clipByUpperObject": true,
                "type": "video",
                "effects": [{
                    "id": "transform",
                    "enabled": true,
                    "known": true,
                    "params": {"x": 1.0, "scale": 125.0, "opacity": 0.75, "tint": "#80402010"},
                    "keyframes": {"x": [
                        {"frame": 0, "value": 0.0, "interp": "linear"},
                        {"frame": 100, "value": 100.0, "interp": "linear"}
                    ]}
                }, {
                    "id": "audio",
                    "enabled": true,
                    "known": false,
                    "params": {"playMode": "direct", "directTime": 3.0, "volume": 0.5, "limiter": true},
                    "keyframes": {}
                }]
            }]
        }))
        .expect("serialize scene fixture")
    }

    #[test]
    fn builds_scene_outputs_and_reuses_numeric_frame_cache() {
        let mut plan = BakePlan::parse(&scene_json()).expect("valid plan");
        assert_eq!(plan.effect_count, 2);
        let key = EvaluationKey { current_frame: 60 };
        let first = plan.compute_frame(key);
        assert_eq!(first.layers.len(), 1);
        assert_eq!(first.audio.len(), 1);
        assert_eq!(first.layers[0].transform.x, 50.0);
        assert_eq!(first.layers[0].transform.scale_percent, 125.0);
        assert_eq!(first.audio[0].direct_time, 3.0);
        assert_eq!(first.audio[0].volume, 0.5);
        let transform = first.layers[0]
            .effects
            .iter()
            .find(|effect| effect.id == "transform")
            .expect("transform effect");
        assert_eq!(transform.params["tint"], json!("#80402010"));

        let repeated = plan.compute_frame(key);
        assert_eq!(repeated.layers.len(), 1);
        assert_eq!(repeated.layers[0].transform.x, 50.0);
    }

    #[test]
    fn references_resolve_to_sibling_parameters_and_values() {
        let bytes = serde_json::to_vec(&json!({
            "fps": 60.0,
            "maxClipId": 16,
            "clips": [{
                "id": 1,
                "layer": 0,
                "startFrame": 0,
                "durationFrames": 120,
                "type": "video",
                "effects": [
                    {
                        "id": "blur",
                        "name": "Blur",
                        "enabled": true,
                        "known": true,
                        "params": {"size": 4.0, "x": 7.0}
                    },
                    {
                        "id": "transform",
                        "enabled": true,
                        "known": true,
                        "params": {
                            "x": "=Blur.$",
                            "y": "=x*2 + 1",
                            "rotationZ": "=Blur.size * 3",
                            "scale": "=@clamp(Blur.size * 10, 0, 20)"
                        }
                    }
                ]
            }]
        }))
        .expect("serialize scene fixture");
        let mut plan = BakePlan::parse(&bytes).expect("valid plan");
        let output = plan.compute_frame(EvaluationKey { current_frame: 0 });
        let transform = output.layers[0]
            .effects
            .iter()
            .find(|effect| effect.id == "transform")
            .expect("transform effect");
        // `$` expands to this parameter's name on the referenced effect.
        assert_eq!(transform.params["x"], json!(7.0));
        assert_eq!(transform.params["y"], json!(15.0));
        // Another effect of the same object is reachable by display name.
        assert_eq!(transform.params["rotationZ"], json!(12.0));
        // `@clamp` is the rule-based alias for the Lua helper.
        assert_eq!(transform.params["scale"], json!(20.0));
    }

    #[test]
    fn unresolvable_references_and_cycles_keep_the_literal_fallback() {
        let bytes = serde_json::to_vec(&json!({
            "fps": 60.0,
            "maxClipId": 16,
            "clips": [{
                "id": 1,
                "layer": 0,
                "startFrame": 0,
                "durationFrames": 60,
                "type": "video",
                "effects": [{
                    "id": "transform",
                    "enabled": true,
                    "known": true,
                    "params": {
                        "x": "=layer1.X + 1",
                        "y": "=missing * 2",
                        "scale": 100.0
                    }
                }]
            }, {
                "id": 2,
                "layer": 1,
                "startFrame": 0,
                "durationFrames": 60,
                "type": "video",
                "effects": [{
                    "id": "blur",
                    "enabled": true,
                    "known": true,
                    "params": {"a": "=b + 1", "b": "=a + 1"}
                }]
            }]
        }))
        .expect("serialize scene fixture");
        let mut plan = BakePlan::parse(&bytes).expect("valid plan");
        let output = plan.compute_frame(EvaluationKey { current_frame: 0 });
        let transform = output.layers[0]
            .effects
            .iter()
            .find(|effect| effect.id == "transform")
            .expect("transform effect");
        assert_eq!(transform.params["x"], json!("=layer1.X + 1"));
        assert_eq!(transform.params["y"], json!("=missing * 2"));
        let blur = output.layers[1]
            .effects
            .iter()
            .find(|effect| effect.id == "blur")
            .expect("blur effect");
        assert_eq!(blur.params["a"], json!("=b + 1"));
        assert_eq!(blur.params["b"], json!("=a + 1"));
    }

    #[test]
    fn references_reach_other_layers_by_number_and_by_name() {
        let bytes = serde_json::to_vec(&json!({
            "fps": 60.0,
            "maxClipId": 32,
            "layerSettings": [{"layer": 1, "name": "Titles"}],
            "clips": [
                {
                    "id": 1,
                    "layer": 0,
                    "startFrame": 0,
                    "durationFrames": 60,
                    "type": "video",
                    "effects": [{
                        "id": "transform",
                        "enabled": true,
                        "known": true,
                        "params": {
                            "x": "=layer1.Blur.size + 1",
                            "y": "=Titles.Blur.size * 2"
                        }
                    }]
                },
                {
                    "id": 2,
                    "layer": 1,
                    "startFrame": 0,
                    "durationFrames": 60,
                    "type": "video",
                    "effects": [{
                        "id": "blur",
                        "name": "Blur",
                        "enabled": true,
                        "known": true,
                        "params": {"size": 4.0}
                    }]
                }
            ]
        }))
        .expect("serialize scene fixture");
        let mut plan = BakePlan::parse(&bytes).expect("valid plan");
        let output = plan.compute_frame(EvaluationKey { current_frame: 0 });
        let transform = output.layers[0]
            .effects
            .iter()
            .find(|effect| effect.id == "transform")
            .expect("transform effect");
        assert_eq!(transform.params["x"], json!(5.0));
        assert_eq!(transform.params["y"], json!(8.0));
    }

    #[test]
    fn above_and_under_count_layers_relative_to_the_referencing_clip() {
        let clips = [0_i32, 2, 4, 6]
            .into_iter()
            .map(|layer| {
                let id = layer + 1;
                let position = f64::from(layer + 1) * 10.0;
                json!({
                    "id": id,
                    "layer": layer,
                    "startFrame": 0,
                    "durationFrames": 60,
                    "type": "video",
                    "effects": if layer == 2 {
                        json!([{
                            "id": "transform",
                            "enabled": true,
                            "known": true,
                            "params": {
                                "x": "=above.x",
                                "y": "=under.x",
                                "z": "=above2.x"
                            }
                        }])
                    } else {
                        json!([{
                            "id": "transform",
                            "enabled": true,
                            "known": true,
                            "params": {"x": position}
                        }])
                    }
                })
            })
            .collect::<Vec<_>>();
        let bytes = serde_json::to_vec(&json!({
            "fps": 60.0,
            "maxClipId": 32,
            "clips": clips
        }))
        .expect("serialize scene fixture");
        let mut plan = BakePlan::parse(&bytes).expect("valid plan");
        let output = plan.compute_frame(EvaluationKey { current_frame: 0 });
        let transform = output
            .layers
            .iter()
            .flat_map(|layer| layer.effects.iter())
            .find(|effect| effect.id == "transform" && effect.params.contains_key("y"))
            .expect("transform effect");
        // Layers 4 and 6 are above layer 2, layer 0 is below it.
        assert_eq!(transform.params["x"], json!(50.0));
        assert_eq!(transform.params["y"], json!(10.0));
        assert_eq!(transform.params["z"], json!(70.0));
    }

    #[test]
    fn expressions_stored_beside_values_keep_the_numeric_fallback() {
        let bytes = serde_json::to_vec(&json!({
            "fps": 60.0,
            "maxClipId": 16,
            "clips": [{
                "id": 1,
                "layer": 0,
                "startFrame": 0,
                "durationFrames": 60,
                "type": "video",
                "effects": [{
                    "id": "blur",
                    "name": "Blur",
                    "enabled": true,
                    "known": true,
                    "params": {"size": 5.0, "quality": 2.0},
                    "expressions": {"quality": "size * 2"},
                    "keyframes": {"size": [
                        {"frame": 0, "value": 5.0, "interp": "linear"},
                        {"frame": 60, "value": 9.0, "interp": "linear"}
                    ]}
                }]
            }]
        }))
        .expect("serialize scene fixture");
        let mut plan = BakePlan::parse(&bytes).expect("valid plan");
        let frame = plan.compute_frame(EvaluationKey { current_frame: 30 });
        let blur = &frame.layers[0].effects[0];
        // The reference follows the keyframe value at this frame (7), not the
        // parameter's stored literal (5).
        assert_eq!(blur.params["quality"], json!(14.0));
    }

    #[test]
    fn reference_substitution_leaves_unknown_text_alone() {
        let mut resolve = |chain: &str| match chain {
            "X" => Some(3.0),
            "$" => Some(0.5),
            "blur.size" => Some(12.0),
            _ => None,
        };
        assert_eq!(substitute_references("X*2 + 1", &mut resolve), "3*2 + 1");
        assert_eq!(
            substitute_references("@clamp(X, 0, 20)", &mut resolve),
            "clamp(3, 0, 20)"
        );
        assert_eq!(
            substitute_references("blur.size - $", &mut resolve),
            "12 - 0.5"
        );
        // Unknown chains and decimal numbers keep their text.
        assert_eq!(
            substitute_references("above.X + 1.5", &mut resolve),
            "above.X + 1.5"
        );
        // A reference chain directly after another one is still separate.
        assert_eq!(substitute_references("X+blur.size", &mut resolve), "3+12");
    }

    #[test]
    fn expression_parameters_are_evaluated_per_frame_with_time_and_keyframe_value() {
        let bytes = serde_json::to_vec(&json!({
            "fps": 60.0,
            "maxClipId": 16,
            "clips": [{
                "id": 1,
                "layer": 0,
                "startFrame": 0,
                "durationFrames": 120,
                "type": "video",
                "effects": [{
                    "id": "transform",
                    "enabled": true,
                    "known": true,
                    "params": {"x": "=time*10", "y": "=value+2", "scale": 100.0},
                    "keyframes": {"y": [
                        {"frame": 0, "value": 0.0, "interp": "linear"},
                        {"frame": 120, "value": 120.0, "interp": "linear"}
                    ]}
                }]
            }]
        }))
        .expect("serialize scene fixture");
        let mut plan = BakePlan::parse(&bytes).expect("valid plan");
        let output = plan.compute_frame(EvaluationKey { current_frame: 60 });
        // `time` is seconds from the object start and `value` is the keyframe result:
        // at frame 60 of a 60 fps scene, x = 1.0 * 10 and y = 60 + 2.
        assert_eq!(output.layers[0].transform.x, 10.0);
        assert_eq!(output.layers[0].transform.y, 62.0);
        // A broken expression keeps the keyframe value instead of dropping the parameter.
        let bytes = serde_json::to_vec(&json!({
            "fps": 60.0,
            "maxClipId": 16,
            "clips": [{
                "id": 2,
                "layer": 0,
                "startFrame": 0,
                "durationFrames": 60,
                "type": "text",
                "effects": [{
                    "id": "transform",
                    "enabled": true,
                    "known": true,
                    "params": {"x": "=1 +", "scale": 100.0},
                    "keyframes": {"x": [
                        {"frame": 0, "value": 7.0, "interp": "linear"},
                        {"frame": 60, "value": 7.0, "interp": "linear"}
                    ]}
                }]
            }]
        }))
        .expect("serialize scene fixture");
        let mut plan = BakePlan::parse(&bytes).expect("valid plan");
        let output = plan.compute_frame(EvaluationKey { current_frame: 30 });
        assert_eq!(output.layers[0].transform.x, 7.0);
    }

    #[test]
    fn a_transform_that_omits_a_parameter_keeps_avutls_value() {
        // Objects saved by an importer, a script, or an older build can carry a
        // transform with only some of its parameters. Reading the absent ones as
        // zero drew the object at scale 0, which is invisible in the preview and in
        // an export while every parameter it does carry looks right.
        let bytes = serde_json::to_vec(&json!({
            "fps": 60.0,
            "maxClipId": 16,
            "clips": [{
                "id": 3,
                "layer": 0,
                "startFrame": 0,
                "durationFrames": 60,
                "clipByUpperObject": false,
                "type": "rect",
                "effects": [{
                    "id": "transform",
                    "enabled": true,
                    "known": true,
                    "params": {"x": 5.0},
                    "keyframes": {}
                }]
            }]
        }))
        .expect("serialize sparse transform fixture");
        let mut plan = BakePlan::parse(&bytes).expect("valid plan");
        let output = plan.compute_frame(EvaluationKey { current_frame: 30 });
        let layer = output.layers.first().expect("the clip renders");
        assert_eq!(layer.transform.x, 5.0);
        assert_eq!(
            layer.transform.scale_percent, 100.0,
            "a missing scale is 100 %, not 0 %"
        );
        assert_eq!(layer.transform.opacity, 1.0, "a missing opacity is opaque");
        assert_eq!(layer.transform.rotation_x, 0.0);
    }

    #[test]
    fn layer_settings_move_media_scale_audio_and_leave_camera_targets_alone() {
        let input = json!({
            "version": 3,
            "settings": {"width": 1920, "height": 1080, "fps": 60, "sampleRate": 48000},
            "scenes": [{
                "id": 1,
                "name": "Scene",
                "duration": 120,
                "layerSettings": [
                    {"layer": 1, "x": 10.0, "y": -5.0, "z": 2.0, "volumePercent": 50},
                    {"layer": 2, "x": 50.0},
                    {"layer": 3, "x": 100.0}
                ]
            }],
            "clips": [
                {"id": 1, "sceneId": 1, "type": "rect", "start": 0, "duration": 60,
                 "layer": 1, "params": {}, "audioPlugins": [],
                 "effects": [{
                    "id": "transform", "name": "Transform", "enabled": true,
                    "params": {"x": 5.0, "y": 1.0, "z": 0.0}
                 }]},
                {"id": 2, "sceneId": 1, "type": "audio", "start": 0, "duration": 60,
                 "layer": 1, "params": {}, "audioPlugins": [],
                 "effects": [{
                    "id": "audio", "name": "Audio", "enabled": true,
                    "params": {"path": "tone.wav", "volume": 2.0}
                 }]},
                {"id": 3, "sceneId": 1, "type": "rect", "start": 0, "duration": 60,
                 "layer": 3, "params": {}, "audioPlugins": [],
                 "effects": [{
                    "id": "transform", "name": "Transform", "enabled": true,
                    "params": {"x": 0.0}
                 }]},
                {"id": 4, "sceneId": 1, "type": "camera_control", "start": 0,
                 "duration": 60, "layer": 2, "params": {}, "audioPlugins": [],
                 "effects": [{
                    "id": "camera_control", "name": "Camera Control", "enabled": true,
                    "params": {"layerCount": 1, "fov": 30.0}
                 }]}
            ]
        });
        let bytes = serde_json::to_vec(&input).expect("serialize project fixture");
        let document = crate::project::parse_project_document(&bytes).expect("project parses");
        let mut plan = SceneRenderPlan::from_document(&document, 1).expect("scene plan builds");
        let frame = plan.evaluate(10);

        let media = frame
            .layers
            .iter()
            .find(|layer| layer.clip_id == 1)
            .expect("media layer renders");
        // The object's own transform plus the layer's screen-space offset.
        assert_eq!(
            (media.transform.x, media.transform.y, media.transform.z),
            (15.0, -4.0, 2.0)
        );

        let audio = frame
            .audio
            .iter()
            .find(|audio| audio.clip_id == 2)
            .expect("audio layer renders");
        assert_eq!(audio.volume, 1.0, "2.0 object volume x 50 % layer volume");

        let camera_target = frame
            .layers
            .iter()
            .find(|layer| layer.clip_id == 3)
            .expect("camera target renders");
        assert_eq!(
            camera_target.transform.x, 0.0,
            "a camera target keeps its own coordinates"
        );

        let camera = frame
            .layers
            .iter()
            .find(|layer| layer.clip_id == 4)
            .expect("camera control renders");
        assert_eq!(
            camera.transform.x, 0.0,
            "control objects are never moved by layer settings"
        );
        assert!(frame.camera.is_some(), "the camera still reaches the scene");
    }

    #[test]
    fn text_control_characters_become_styled_runs_in_the_frame_plan() {
        let input = json!({
            "version": 3,
            "settings": {"width": 1920, "height": 1080, "fps": 60, "sampleRate": 48000},
            "scenes": [{"id": 1, "name": "Scene", "duration": 120}],
            "clips": [{
                "id": 1,
                "sceneId": 1,
                "type": "text",
                "start": 0,
                "duration": 60,
                "layer": 0,
                "params": {},
                "audioPlugins": [],
                "effects": [{
                    "id": "text",
                    "name": "Text",
                    "enabled": true,
                    "params": {
                        "text": "A<#ff0000>red<s96>big",
                        "fontSize": 48,
                        "color": "#ffffff"
                    }
                }]
            }]
        });
        let bytes = serde_json::to_vec(&input).expect("serialize project fixture");
        let document = crate::project::parse_project_document(&bytes).expect("project parses");
        let mut plan = SceneRenderPlan::from_document(&document, 1).expect("scene plan builds");
        let frame = plan.evaluate(0);
        let text = frame
            .layers
            .iter()
            .find(|layer| layer.clip_id == 1)
            .and_then(|layer| layer.text.as_ref())
            .expect("text layer renders");

        assert_eq!(text.content, "A<#ff0000>red<s96>big");
        assert_eq!(text.runs.len(), 3);
        assert_eq!(text.runs[0].text, "A");
        assert_eq!(
            (
                text.runs[0].color.red,
                text.runs[0].color.green,
                text.runs[0].color.blue
            ),
            (0xff, 0xff, 0xff)
        );
        assert_eq!(text.runs[1].text, "red");
        assert_eq!(
            (
                text.runs[1].color.red,
                text.runs[1].color.green,
                text.runs[1].color.blue
            ),
            (0xff, 0x00, 0x00)
        );
        assert_eq!(text.runs[2].text, "big");
        assert_eq!(text.runs[2].font_size, 96.0);
        // The style directives are not part of the drawn text.
        assert!(
            text.runs
                .iter()
                .all(|run| !run.text.contains('<') && !run.text.contains('>'))
        );
    }

    #[test]
    fn text_speed_and_clear_directives_follow_the_frame() {
        let input = json!({
            "version": 3,
            "settings": {"width": 1920, "height": 1080, "fps": 10, "sampleRate": 48000},
            "scenes": [{"id": 1, "name": "Scene", "duration": 120}],
            "clips": [{
                "id": 1,
                "sceneId": 1,
                "type": "text",
                "start": 0,
                "duration": 60,
                "layer": 0,
                "params": {},
                "audioPlugins": [],
                "effects": [{
                    "id": "text",
                    "name": "Text",
                    "enabled": true,
                    "params": {
                        "text": "abcd",
                        "fontSize": 48,
                        "speed": 2
                    }
                }]
            }, {
                "id": 2,
                "sceneId": 1,
                "type": "text",
                "start": 0,
                "duration": 60,
                "layer": 1,
                "params": {},
                "audioPlugins": [],
                "effects": [{
                    "id": "text",
                    "name": "Text",
                    "enabled": true,
                    "params": {"text": "one<c1>two", "fontSize": 48}
                }]
            }]
        });
        let bytes = serde_json::to_vec(&input).expect("serialize project fixture");
        let document = crate::project::parse_project_document(&bytes).expect("project parses");
        let mut plan = SceneRenderPlan::from_document(&document, 1).expect("scene plan builds");

        let runs_at = |plan: &mut SceneRenderPlan, frame, clip_id| {
            plan.evaluate(frame)
                .layers
                .iter()
                .find(|layer| layer.clip_id == clip_id)
                .and_then(|layer| layer.text.as_ref())
                .map(|text| {
                    text.runs
                        .iter()
                        .map(|run| run.text.as_str())
                        .collect::<String>()
                })
                .unwrap_or_default()
        };

        // Two characters per second at 10 fps: one at 0 s, two at 0.5 s.
        assert_eq!(runs_at(&mut plan, 0, 1), "a");
        assert_eq!(runs_at(&mut plan, 5, 1), "ab");
        assert_eq!(runs_at(&mut plan, 10, 1), "abc");

        // The clear swaps the visible block at one second.
        assert_eq!(runs_at(&mut plan, 5, 2), "one");
        assert_eq!(runs_at(&mut plan, 10, 2), "two");
    }

    #[test]
    fn a_text_script_can_rotate_the_object_for_the_frame() {
        let input = json!({
            "version": 3,
            "settings": {"width": 1920, "height": 1080, "fps": 10, "sampleRate": 48000},
            "scenes": [{"id": 1, "name": "Scene", "duration": 120}],
            "clips": [{
                "id": 1,
                "sceneId": 1,
                "type": "text",
                "start": 0,
                "duration": 60,
                "layer": 0,
                "params": {},
                "audioPlugins": [],
                "effects": [{
                    "id": "text",
                    "name": "Text",
                    "enabled": true,
                    "params": {"text": "<?obj.rz=obj.time*360?>spin", "fontSize": 48}
                }, {
                    "id": "transform",
                    "name": "Transform",
                    "enabled": true,
                    "params": {"x": 100, "rotationZ": 0}
                }]
            }]
        });
        let bytes = serde_json::to_vec(&input).expect("serialize project fixture");
        let document = crate::project::parse_project_document(&bytes).expect("project parses");
        let mut plan = SceneRenderPlan::from_document(&document, 1).expect("scene plan builds");
        let layer_at = |plan: &mut SceneRenderPlan, frame: i32| {
            plan.evaluate(frame)
                .layers
                .iter()
                .find(|layer| layer.clip_id == 1)
                .map(|layer| layer.transform)
                .expect("the text layer renders")
        };
        // 10 fps: frame 5 is half a second, so the script rotates by 180°.
        let half = layer_at(&mut plan, 5);
        assert_eq!(half.rotation_z, 180.0);
        assert_eq!(half.x, 100.0, "the object's own x is unchanged");
        let start = layer_at(&mut plan, 0);
        assert_eq!(start.rotation_z, 0.0, "the script follows the frame");
        assert_eq!(layer_at(&mut plan, 10).rotation_z, 360.0);
    }

    #[test]
    fn text_scripts_expand_before_control_characters_are_parsed() {
        let input = json!({
            "version": 3,
            "settings": {"width": 1920, "height": 1080, "fps": 10, "sampleRate": 48000},
            "scenes": [{"id": 1, "name": "Scene", "duration": 120}],
            "clips": [{
                "id": 1,
                "sceneId": 1,
                "type": "text",
                "start": 0,
                "duration": 60,
                "layer": 0,
                "params": {},
                "audioPlugins": [],
                "effects": [{
                    "id": "text",
                    "name": "Text",
                    "enabled": true,
                    "params": {
                        "text": "A<?=2+3?>B<#ff0000>C",
                        "fontSize": 48,
                        "color": "#ffffff"
                    }
                }]
            }, {
                "id": 2,
                "sceneId": 1,
                "type": "text",
                "start": 0,
                "duration": 60,
                "layer": 1,
                "params": {},
                "audioPlugins": [],
                "effects": [{
                    "id": "text",
                    "name": "Text",
                    "enabled": true,
                    "params": {
                        "text": "<? mes(math.floor(obj.time)) ?>/<? mes(obj.totalframe) ?>/<? mes(obj.layer) ?>/<? mes(obj.frame_s) ?>/<? mes(obj.screen_w) ?>/<? mes(obj.scene_id) ?>",
                        "fontSize": 48
                    }
                }]
            }]
        });
        let bytes = serde_json::to_vec(&input).expect("serialize project fixture");
        let document = crate::project::parse_project_document(&bytes).expect("project parses");
        let mut plan = SceneRenderPlan::from_document(&document, 1).expect("scene plan builds");
        let frame = plan.evaluate(0);
        let text = frame
            .layers
            .iter()
            .find(|layer| layer.clip_id == 1)
            .and_then(|layer| layer.text.as_ref())
            .expect("text layer renders");
        assert_eq!(text.content, "A5B<#ff0000>C");
        assert_eq!(text.runs.len(), 2);
        assert_eq!(text.runs[0].text, "A5B");
        assert_eq!(text.runs[1].text, "C");
        assert_eq!(
            (
                text.runs[1].color.red,
                text.runs[1].color.green,
                text.runs[1].color.blue
            ),
            (0xff, 0x00, 0x00)
        );

        // At 10 fps the 15th frame is 1.5 s, so the script prints 1 of 60; the
        // object context supplies its layer, start frame, screen and scene.
        let frame = plan.evaluate(15);
        let text = frame
            .layers
            .iter()
            .find(|layer| layer.clip_id == 2)
            .and_then(|layer| layer.text.as_ref())
            .expect("script text layer renders");
        assert_eq!(text.content, "1/60/1/0/1920/1");
    }

    #[test]
    fn typed_audio_plan_evaluates_plugin_parameters_and_preserves_host_metadata() {
        let input = json!({
            "version": 3,
            "settings": {"width": 1920, "height": 1080, "fps": 60, "sampleRate": 48000},
            "scenes": [{"id": 1, "name": "Scene", "duration": 120}],
            "clips": [{
                "id": 7,
                "sceneId": 1,
                "type": "audio",
                "start": 10,
                "duration": 20,
                "layer": 0,
                "params": {},
                "audioPlugins": [{
                    "id": "CLAP:org.example.gain:0",
                    "enabled": true,
                    "params": {"0": 0.25, "1": true},
                    "keyframes": {"0": {
                        "start": {"frame": 0, "value": 0.25, "interp": "linear"},
                        "points": [{"frame": 10, "value": 0.75, "interp": "linear"}]
                    }},
                    "hostBackend": "truce-rack",
                    "nativeId": "org.example.gain",
                    "path": "/plugins/gain.clap"
                }],
                "effects": [{
                    "id": "audio",
                    "name": "Audio",
                    "enabled": true,
                    "params": {"path": "tone.wav"}
                }]
            }]
        });
        let bytes = serde_json::to_vec(&input).expect("serialize project fixture");
        let document = crate::project::parse_project_document(&bytes).expect("project parses");
        let mut plan = SceneRenderPlan::from_document(&document, 1).expect("scene plan builds");

        let frame = plan.evaluate(15);
        assert_eq!(frame.audio.len(), 1);
        assert_eq!(frame.audio[0].plugins.len(), 1);
        let plugin = &frame.audio[0].plugins[0];
        assert_eq!(plugin.id, "CLAP:org.example.gain:0");
        assert!(plugin.enabled);
        assert_eq!(plugin.params["0"], json!(0.5));
        assert_eq!(plugin.params["1"], json!(true));
        assert_eq!(plugin.extra["hostBackend"], json!("truce-rack"));
        assert_eq!(plugin.extra["nativeId"], json!("org.example.gain"));
        assert_eq!(plugin.extra["path"], json!("/plugins/gain.clap"));
    }

    #[test]
    fn typed_frame_renders_counter_and_simple_shape_objects() {
        let input = serde_json::to_vec(&json!({
            "fps": 60.0,
            "clips": [{
                "id": 1,
                "layer": 0,
                "durationFrames": 120,
                "type": "counter",
                "effects": [{
                    "id": "counter",
                    "known": true,
                    "params": {
                        "mode": "value",
                        "startValue": 10,
                        "endValue": 110,
                        "digits": 4,
                        "decimals": 1,
                        "prefix": "[",
                        "suffix": "]",
                        "fontSize": 64,
                        "opacity": 0.5
                    }
                }]
            }, {
                "id": 2,
                "layer": 1,
                "durationFrames": 120,
                "type": "polygon_shape",
                "effects": [{
                    "id": "polygon_shape",
                    "known": true,
                    "params": {"sizeW": 320, "sizeH": 240, "sides": 6, "rotation": 15}
                }]
            }, {
                "id": 3,
                "layer": 2,
                "durationFrames": 120,
                "type": "pie_shape",
                "effects": [{
                    "id": "pie_shape",
                    "known": true,
                    "params": {"sizeW": 400, "sizeH": 300, "angle": 123.5}
                }]
            }]
        }))
        .expect("serialize custom-object fixture");
        let mut plan = BakePlan::parse(&input).expect("valid custom-object plan");
        let frame = plan.compute_frame(EvaluationKey { current_frame: 30 });

        let counter = frame
            .layers
            .iter()
            .find(|layer| layer.clip_id == 1)
            .and_then(|layer| layer.text.as_ref())
            .expect("counter produces a text render plan");
        assert_eq!(counter.content, "[0035.0]");
        assert_eq!(counter.font_size, 64.0);
        assert_eq!(counter.opacity, 0.5);
        assert!(counter.outline_enabled);

        let polygon = frame
            .layers
            .iter()
            .find(|layer| layer.clip_id == 2)
            .and_then(|layer| layer.shape.as_ref())
            .expect("polygon object produces a shape render plan");
        assert_eq!(polygon.kind, ShapeKind::Polygon);
        assert_eq!(
            (polygon.width, polygon.height, polygon.sides),
            (320.0, 240.0, 6)
        );
        assert_eq!(polygon.rotation_degrees, 15.0);
        assert!(!polygon.normalize_vertices);
        assert!(!polygon.even_sides_half_step);
        assert_eq!(polygon.edge_padding, 4.0);

        let pie = frame
            .layers
            .iter()
            .find(|layer| layer.clip_id == 3)
            .and_then(|layer| layer.shape.as_ref())
            .expect("pie object produces a shape render plan");
        assert_eq!(pie.kind, ShapeKind::Pie);
        assert_eq!(pie.sweep_degrees, 123.5);
        assert_eq!((pie.width, pie.height), (400.0, 300.0));
    }

    #[test]
    fn counter_modes_and_padding_match_the_qml_contract() {
        let mut params = BTreeMap::from([
            ("digits".to_owned(), json!(3)),
            ("decimals".to_owned(), json!(2)),
            ("prefix".to_owned(), json!("T=")),
            ("suffix".to_owned(), json!("s")),
            ("mode".to_owned(), json!("time")),
        ]);
        assert_eq!(counter_text(&params, 75, 120, 60.0), "T=001.25s");

        params.insert("mode".to_owned(), json!("frame"));
        params.insert("decimals".to_owned(), json!(0));
        assert_eq!(counter_text(&params, 7, 120, 60.0), "T=007s");

        params.insert("mode".to_owned(), json!("value"));
        params.insert("startValue".to_owned(), json!(-10));
        params.insert("endValue".to_owned(), json!(10));
        params.insert("digits".to_owned(), json!(2));
        assert_eq!(counter_text(&params, 30, 120, 60.0), "T=-05s");
    }

    #[test]
    fn typed_frame_renders_the_remaining_canvas_objects() {
        let clips = [
            (4, "track_line", json!({"lineWidth": 12, "arrow": false})),
            (5, "star", json!({"count": 25, "speed": 2, "seed": 9})),
            (
                6,
                "radial_lines",
                json!({"lineCount": 32, "spinSpeed": 0.5, "seed": 3}),
            ),
            (7, "flare", json!({"radius": 90, "ghosts": 0})),
            (8, "lens_flare_object", json!({"radius": 180, "ghosts": 4})),
        ]
        .into_iter()
        .map(|(id, clip_type, params)| {
            json!({
                "id": id,
                "layer": id,
                "durationFrames": 120,
                "type": clip_type,
                "effects": [{"id": clip_type, "known": true, "params": params}]
            })
        })
        .collect::<Vec<_>>();
        let input = serde_json::to_vec(&json!({"fps": 60.0, "clips": clips}))
            .expect("serialize Canvas-object fixture");
        let mut plan = BakePlan::parse(&input).expect("valid Canvas-object plan");
        let frame = plan.compute_frame(EvaluationKey { current_frame: 10 });
        assert_eq!(frame.layers.len(), 5);

        let procedural = |clip_id| {
            frame
                .layers
                .iter()
                .find(|layer| layer.clip_id == clip_id)
                .and_then(|layer| layer.procedural.as_ref())
                .expect("clip produces a procedural render plan")
        };
        let ProceduralObjectRenderPlan::TrackLine(track) = procedural(4) else {
            panic!("track line plan kind");
        };
        assert_eq!(track.line_width, 12.0);
        assert!(!track.arrow);

        let ProceduralObjectRenderPlan::ParticleField(stars) = procedural(5) else {
            panic!("particle field plan kind");
        };
        assert_eq!((stars.count, stars.seed, stars.relative_frame), (25, 9, 10));

        let ProceduralObjectRenderPlan::RadialLines(lines) = procedural(6) else {
            panic!("radial lines plan kind");
        };
        assert_eq!(
            (lines.line_count, lines.seed, lines.relative_frame),
            (32, 3, 10)
        );

        let ProceduralObjectRenderPlan::LensFlare(flare) = procedural(7) else {
            panic!("flare plan kind");
        };
        assert_eq!((flare.radius, flare.ghosts), (90.0, 0));

        let ProceduralObjectRenderPlan::LensFlare(lens_flare) = procedural(8) else {
            panic!("lens flare plan kind");
        };
        assert_eq!((lens_flare.radius, lens_flare.ghosts), (180.0, 4));
    }

    #[test]
    fn group_controls_apply_to_the_same_following_layer_range_as_qml() {
        let input = serde_json::to_vec(&json!({
            "fps": 60.0,
            "clips": [{
                "id": 1,
                "layer": 1,
                "durationFrames": 120,
                "type": "GroupControl",
                "effects": [{
                    "id": "GroupControl",
                    "known": true,
                    "params": {
                        "layerCount": 1,
                        "x": 100,
                        "y": 20,
                        "z": 5,
                        "scale": 200,
                        "rotationZ": 90,
                        "opacity": 0.5
                    }
                }]
            }, {
                "id": 2,
                "layer": 2,
                "durationFrames": 120,
                "type": "rect",
                "effects": [{
                    "id": "transform",
                    "known": true,
                    "params": {"x": 10, "y": 0, "z": 2, "rotationZ": 5, "opacity": 0.8}
                }, {
                    "id": "rect",
                    "known": true,
                    "params": {"sizeW": 20, "sizeH": 20}
                }]
            }, {
                "id": 3,
                "layer": 3,
                "durationFrames": 120,
                "type": "rect",
                "effects": [{
                    "id": "transform",
                    "known": true,
                    "params": {"x": 10, "opacity": 0.8}
                }, {
                    "id": "rect",
                    "known": true,
                    "params": {"sizeW": 20, "sizeH": 20}
                }]
            }]
        }))
        .expect("serialize group-control fixture");
        let mut plan = BakePlan::parse(&input).expect("valid group-control plan");
        let frame = plan.compute_frame(EvaluationKey { current_frame: 0 });
        let controlled = frame
            .layers
            .iter()
            .find(|layer| layer.clip_id == 2)
            .expect("controlled layer");
        assert!((controlled.transform.x - 100.0).abs() < 0.001);
        assert!((controlled.transform.y - 40.0).abs() < 0.001);
        assert!((controlled.transform.z - 9.0).abs() < 0.001);
        assert_eq!(controlled.transform.rotation_z, 95.0);
        assert!((controlled.transform.opacity - 0.4).abs() < 0.001);

        let outside_range = frame
            .layers
            .iter()
            .find(|layer| layer.clip_id == 3)
            .expect("outside layer");
        assert_eq!(outside_range.transform.x, 10.0);
        assert_eq!(outside_range.transform.rotation_z, 0.0);
        assert!((outside_range.transform.opacity - 0.8).abs() < 0.001);
    }

    #[test]
    fn lowest_active_camera_control_owns_the_scene_camera() {
        let cameras = [(1, 5, 10), (2, 2, 30)]
            .into_iter()
            .map(|(id, layer, z)| {
                json!({
                    "id": id,
                    "layer": layer,
                    "durationFrames": 120,
                    "type": "camera_control",
                    "effects": [{
                        "id": "camera_control",
                        "known": true,
                        "params": {
                            "x": 10,
                            "y": 20,
                            "z": z,
                            "tx": -30,
                            "ty": -40,
                            "tz": 50,
                            "roll": 12,
                            "fov": 60
                        }
                    }]
                })
            })
            .collect::<Vec<_>>();
        let input = serde_json::to_vec(&json!({"fps": 60.0, "height": 720, "clips": cameras}))
            .expect("serialize camera-control fixture");
        let mut plan = BakePlan::parse(&input).expect("valid camera-control plan");
        let camera = plan
            .compute_frame(EvaluationKey { current_frame: 0 })
            .camera
            .expect("active camera control");
        assert_eq!((camera.position_x, camera.position_y), (10.0, -20.0));
        assert!((camera.position_z - 653.5383).abs() < 0.001);
        assert_eq!(
            (camera.target_x, camera.target_y, camera.target_z),
            (-30.0, 40.0, 50.0)
        );
        assert_eq!(camera.roll_degrees, 12.0);
        assert_eq!(camera.field_of_view_degrees, 60.0);
    }

    #[test]
    fn camera_control_parameters_fall_back_to_the_legacy_camera_effect() {
        let input = serde_json::to_vec(&json!({
            "fps": 60.0,
            "height": 720,
            "clips": [{
                "id": 1,
                "layer": 1,
                "durationFrames": 120,
                "type": "camera",
                "effects": [{
                    "id": "camera",
                    "known": true,
                    "params": {"x": 40, "y": 20, "fov": 60}
                }, {
                    "id": "camera_control",
                    "known": true,
                    "params": {"x": 10}
                }]
            }]
        }))
        .expect("serialize legacy camera fixture");
        let mut plan = BakePlan::parse(&input).expect("valid legacy camera plan");
        let camera = plan
            .compute_frame(EvaluationKey { current_frame: 0 })
            .camera
            .expect("legacy camera control");
        assert_eq!((camera.position_x, camera.position_y), (10.0, -20.0));
        assert_eq!(camera.field_of_view_degrees, 60.0);
    }

    #[test]
    fn on_demand_selection_uses_temporal_buckets() {
        let input = serde_json::to_vec(&json!({
            "fps": 60.0,
            "clips": (0..1000).map(|index| json!({
                "id": index,
                "startFrame": index * 240,
                "durationFrames": 30
            })).collect::<Vec<_>>()
        }))
        .expect("serialize bucket fixture");
        let mut plan = BakePlan::parse(&input).expect("valid plan");
        let result = plan.compute_frame(EvaluationKey {
            current_frame: 500 * 240 + 5,
        });
        assert_eq!(result.layers.len(), 1);
        assert_eq!(result.layers[0].clip_id, 500);
    }
}
