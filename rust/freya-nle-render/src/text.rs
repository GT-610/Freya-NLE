use fontdb::{Database, Family, ID, Query, Stretch, Style, Weight};
use freya_nle_core::api::{RgbaColor, RubyRun, TextAlignment, TextAnchor, TextRenderPlan};
use freya_nle_media::VideoFrame;
use rustybuzz::ttf_parser::{GlyphId, OutlineBuilder};
use rustybuzz::{Face, UnicodeBuffer};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use tiny_skia::{
    Color, FillRule, LineJoin, Paint, Path, PathBuilder, Pixmap, PixmapPaint, Rect, Stroke,
    Transform,
};

/// The widest per-run `<s...,outline>` value that is accepted.
const MAX_RUN_OUTLINE_WIDTH: f32 = 512.0;

const MAX_RASTER_DIMENSION: f32 = 8_192.0;
const MAX_TEXT_BYTES: usize = 1_000_000;
const FALLBACK_FAMILIES: &[&str] = &[
    "Hiragino Sans",
    "Noto Sans CJK JP",
    "Noto Sans",
    "Yu Gothic",
    "Meiryo",
    "Microsoft YaHei",
    "Arial Unicode MS",
];

/// Failure to turn an evaluated text object into a bounded RGBA frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextRasterError {
    InvalidParameters,
    FontNotFound,
    InvalidFont,
    InvalidDimensions,
    InvalidPath,
    AllocationFailed,
}

impl Display for TextRasterError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidParameters => "text parameters are invalid",
            Self::FontNotFound => "no usable system font was found",
            Self::InvalidFont => "the selected font could not be parsed",
            Self::InvalidDimensions => "text dimensions are invalid or exceed 8192 pixels",
            Self::InvalidPath => "text glyph outlines could not be constructed",
            Self::AllocationFailed => "text pixel buffer could not be allocated",
        })
    }
}

impl Error for TextRasterError {}

/// Owns the platform font catalog used by the Rust preview worker.
/// One face's bytes plus the index inside those bytes.
type FaceBytes = (Arc<[u8]>, u32);

pub struct TextRasterizer {
    database: Database,
    /// Bytes of every face a run asked for, so a styled run does not re-read
    /// the same font file for each glyph batch.
    face_bytes: RefCell<HashMap<ID, FaceBytes>>,
}

impl TextRasterizer {
    /// Loads the platform font directories once for subsequent text frames.
    ///
    /// No `Default` impl by design: construction scans the system font
    /// catalog, so call sites spell out `new()`.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let mut database = Database::new();
        database.load_system_fonts();
        Self {
            database,
            face_bytes: RefCell::new(HashMap::new()),
        }
    }

    /// Rasterizes one evaluated built-in text object into straight-alpha RGBA pixels.
    pub fn rasterize(
        &self,
        plan: &TextRenderPlan,
        timestamp_seconds: f64,
    ) -> Result<VideoFrame, TextRasterError> {
        validate_plan(plan)?;
        if plan.runs_authoritative || plan.runs.len() > 1 {
            return self.rasterize_styled_runs(plan, timestamp_seconds);
        }
        let face_id = self
            .select_face_for(
                plan.font_family.trim(),
                plan.bold,
                plan.italic,
                &plan.content,
            )
            .ok_or(TextRasterError::FontNotFound)?;
        self.database
            .with_face_data(face_id, |data, face_index| {
                rasterize_with_face(plan, timestamp_seconds, data, face_index)
            })
            .ok_or(TextRasterError::InvalidFont)?
    }

    /// Loads one face per run and rasterizes the runs that asked for their own
    /// family or style.
    fn rasterize_styled_runs(
        &self,
        plan: &TextRenderPlan,
        timestamp_seconds: f64,
    ) -> Result<VideoFrame, TextRasterError> {
        let mut loaded = Vec::with_capacity(plan.runs.len());
        for run in &plan.runs {
            let family = run
                .font_family
                .as_deref()
                .unwrap_or(plan.font_family.as_str());
            let face = self
                .select_face_for(family, run.bold, run.italic, &run.text)
                .ok_or(TextRasterError::FontNotFound)?;
            loaded.push(self.load_face(face)?);
        }
        let faces = loaded
            .iter()
            .map(|(bytes, index)| {
                Face::from_slice(bytes, *index).ok_or(TextRasterError::InvalidFont)
            })
            .collect::<Result<Vec<_>, _>>()?;
        rasterize_runs(plan, timestamp_seconds, &faces)
    }

    /// Copies a face's bytes out of the database once.
    ///
    /// The layout needs every run's face at the same time, and `fontdb` only
    /// lends one at a time, so the bytes are cached here instead.
    fn load_face(&self, id: ID) -> Result<FaceBytes, TextRasterError> {
        if let Some(cached) = self.face_bytes.borrow().get(&id) {
            return Ok(cached.clone());
        }
        let loaded = self
            .database
            .with_face_data(id, |data, index| (Arc::from(data), index))
            .ok_or(TextRasterError::FontNotFound)?;
        self.face_bytes.borrow_mut().insert(id, loaded.clone());
        Ok(loaded)
    }

    fn select_face_for(&self, family: &str, bold: bool, italic: bool, text: &str) -> Option<ID> {
        let weight = if bold { Weight::BOLD } else { Weight::NORMAL };
        let style = if italic { Style::Italic } else { Style::Normal };
        let requested = requested_family(family.trim());
        let mut families = vec![requested];
        if requested != Family::SansSerif {
            families.push(Family::SansSerif);
        }
        let query = Query {
            families: &families,
            weight,
            stretch: Stretch::Normal,
            style,
        };
        let primary = self.database.query(&query);
        if primary.is_some_and(|id| self.face_supports_text(id, text)) {
            return primary;
        }

        for family in FALLBACK_FAMILIES {
            let named = [Family::Name(family)];
            let query = Query {
                families: &named,
                weight,
                stretch: Stretch::Normal,
                style,
            };
            if let Some(id) = self.database.query(&query)
                && self.face_supports_text(id, text)
            {
                return Some(id);
            }
        }

        let mut candidates = self
            .database
            .faces()
            .map(|face| face.id)
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            let left = self
                .database
                .face(*left)
                .expect("font ID came from this database");
            let right = self
                .database
                .face(*right)
                .expect("font ID came from this database");
            face_match_score(left.style, left.weight, style, weight)
                .cmp(&face_match_score(right.style, right.weight, style, weight))
                .then_with(|| left.post_script_name.cmp(&right.post_script_name))
        });
        candidates
            .into_iter()
            .find(|id| self.face_supports_text(*id, text))
            .or(primary)
    }

    fn face_supports_text(&self, id: ID, text: &str) -> bool {
        self.database
            .with_face_data(id, |data, face_index| {
                Face::from_slice(data, face_index).is_some_and(|face| {
                    text.chars().all(|character| {
                        ignorable_for_font_matching(character)
                            || face.glyph_index(character).is_some()
                    })
                })
            })
            .unwrap_or(false)
    }
}

fn requested_family(name: &str) -> Family<'_> {
    if name.eq_ignore_ascii_case("serif") {
        Family::Serif
    } else if name.eq_ignore_ascii_case("sans-serif") || name.is_empty() {
        Family::SansSerif
    } else if name.eq_ignore_ascii_case("cursive") {
        Family::Cursive
    } else if name.eq_ignore_ascii_case("fantasy") {
        Family::Fantasy
    } else if name.eq_ignore_ascii_case("monospace") {
        Family::Monospace
    } else {
        Family::Name(name)
    }
}

fn face_match_score(
    actual_style: Style,
    actual_weight: Weight,
    requested_style: Style,
    requested_weight: Weight,
) -> (u16, u16) {
    let style_penalty = if actual_style == requested_style {
        0
    } else {
        1
    };
    (style_penalty, actual_weight.0.abs_diff(requested_weight.0))
}

/// The widest outline any run asks for, or the plan's own width.
fn run_outline_width(plan: &TextRenderPlan, widths: impl Iterator<Item = Option<f32>>) -> f32 {
    widths.flatten().fold(plan.outline_width, f32::max)
}

fn ignorable_for_font_matching(character: char) -> bool {
    character.is_whitespace()
        || character.is_control()
        || character == '\u{200d}'
        || ('\u{fe00}'..='\u{fe0f}').contains(&character)
        || ('\u{e0100}'..='\u{e01ef}').contains(&character)
}

fn validate_plan(plan: &TextRenderPlan) -> Result<(), TextRasterError> {
    let finite = [
        plan.font_size,
        plan.letter_spacing,
        plan.line_spacing,
        plan.outline_width,
        plan.shadow_offset_x,
        plan.shadow_offset_y,
        plan.background_radius,
        plan.background_padding_x,
        plan.background_padding_y,
    ]
    .into_iter()
    .all(f32::is_finite);
    let runs_finite = plan.runs.iter().all(|run| {
        run.outline_width
            .is_none_or(|width| width.is_finite() && (0.0..=MAX_RUN_OUTLINE_WIDTH).contains(&width))
    });
    if !finite
        || !runs_finite
        || plan.content.len() > MAX_TEXT_BYTES
        || plan.font_size <= 0.0
        || plan.line_spacing < 0.0
        || plan.outline_width < 0.0
        || plan.background_radius < 0.0
        || plan.background_padding_x < 0.0
        || plan.background_padding_y < 0.0
    {
        return Err(TextRasterError::InvalidParameters);
    }
    Ok(())
}

#[derive(Debug)]
struct GlyphPlacement {
    glyph_id: u16,
    x: f32,
    y_offset: f32,
}

#[derive(Debug)]
struct LineLayout {
    glyphs: Vec<GlyphPlacement>,
    width: f32,
    /// Glyph indices that are space characters, in ascending order. A
    /// justified line inserts its extra advance after each of them.
    spaces: Vec<usize>,
}

fn rasterize_with_face(
    plan: &TextRenderPlan,
    timestamp_seconds: f64,
    data: &[u8],
    face_index: u32,
) -> Result<VideoFrame, TextRasterError> {
    let face = Face::from_slice(data, face_index).ok_or(TextRasterError::InvalidFont)?;
    // Styled runs take the `rasterize_runs` path in `rasterize`; this stays in
    // charge of plain text so its layout, justification and metrics do not
    // change for existing projects.
    let units_per_em = face.units_per_em().max(1) as f32;
    let scale = plan.font_size / units_per_em;
    let ascent = (f32::from(face.ascender()) * scale).max(plan.font_size * 0.5);
    let descent = (-f32::from(face.descender()) * scale).max(plan.font_size * 0.1);
    let glyph_line_height = (ascent + descent).max(plan.font_size);
    let natural_line_advance =
        (glyph_line_height + f32::from(face.line_gap()) * scale).max(glyph_line_height);
    let line_advance = if plan.line_spacing > 0.0 {
        plan.line_spacing
    } else {
        natural_line_advance
    };
    let line_texts = plan
        .content
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect::<Vec<_>>();
    let mut lines = line_texts
        .iter()
        .map(|line| shape_line(&face, line, scale, plan.letter_spacing))
        .collect::<Vec<_>>();
    let logical_width = lines.iter().map(|line| line.width).fold(0.0_f32, f32::max);
    if plan.alignment == TextAlignment::Justify {
        // Qt justifies every line of a paragraph except its last one.
        for index in 0..lines.len().saturating_sub(1) {
            if let Some(glyphs) = justified_glyphs(
                &face,
                line_texts[index],
                scale,
                plan.letter_spacing,
                logical_width,
            ) {
                lines[index] = LineLayout {
                    glyphs,
                    width: logical_width,
                    spaces: Vec::new(),
                };
            }
        }
    }
    let logical_height = glyph_line_height + line_advance * (lines.len().saturating_sub(1) as f32);
    if !logical_width.is_finite() || !logical_height.is_finite() {
        return Err(TextRasterError::InvalidDimensions);
    }

    let mut path_builder = PathBuilder::new();
    let mut outlined_glyphs = 0_usize;
    for (line_index, line) in lines.iter().enumerate() {
        let x = alignment_offset(plan.alignment, logical_width, line.width);
        let baseline = ascent + line_index as f32 * line_advance;
        for glyph in &line.glyphs {
            let mut outline = GlyphOutline {
                path: &mut path_builder,
                origin_x: x + glyph.x,
                baseline_y: baseline - glyph.y_offset,
                scale,
                x_scale: 1.0,
                y_scale: 1.0,
                pivot_x: 0.0,
                pivot_y: 0.0,
                cos: 1.0,
                sin: 0.0,
            };
            if face
                .outline_glyph(GlyphId(glyph.glyph_id), &mut outline)
                .is_some()
            {
                outlined_glyphs += 1;
            }
        }
    }
    let has_visible_text = plan
        .content
        .chars()
        .any(|character| !ignorable_for_font_matching(character));
    if has_visible_text && outlined_glyphs == 0 {
        return Err(TextRasterError::InvalidPath);
    }
    let path = path_builder.finish();
    let (visual_left, visual_top, visual_right, visual_bottom) =
        path.as_ref()
            .map_or((0.0, 0.0, logical_width, logical_height), |path| {
                let bounds = path.bounds();
                (
                    bounds.left().min(0.0),
                    bounds.top().min(0.0),
                    bounds.right().max(logical_width),
                    bounds.bottom().max(logical_height),
                )
            });
    let visual_width = (visual_right - visual_left).max(0.0);
    let visual_height = (visual_bottom - visual_top).max(0.0);
    let outline_padding = if plan.outline_enabled {
        plan.outline_width.ceil() + 2.0
    } else {
        2.0
    };
    let background_padding_x = if plan.background_enabled {
        plan.background_padding_x
    } else {
        0.0
    };
    let background_padding_y = if plan.background_enabled {
        plan.background_padding_y
    } else {
        0.0
    };
    let base_width = visual_width + (outline_padding + background_padding_x) * 2.0;
    let base_height = visual_height + (outline_padding + background_padding_y) * 2.0;
    let shadow_left = if plan.shadow_enabled {
        (-plan.shadow_offset_x).max(0.0)
    } else {
        0.0
    };
    let shadow_right = if plan.shadow_enabled {
        plan.shadow_offset_x.max(0.0)
    } else {
        0.0
    };
    let shadow_top = if plan.shadow_enabled {
        (-plan.shadow_offset_y).max(0.0)
    } else {
        0.0
    };
    let shadow_bottom = if plan.shadow_enabled {
        plan.shadow_offset_y.max(0.0)
    } else {
        0.0
    };
    let raster_width = (base_width + shadow_left + shadow_right).ceil().max(1.0);
    let raster_height = (base_height + shadow_top + shadow_bottom).ceil().max(1.0);
    if !raster_width.is_finite()
        || !raster_height.is_finite()
        || raster_width > MAX_RASTER_DIMENSION
        || raster_height > MAX_RASTER_DIMENSION
    {
        return Err(TextRasterError::InvalidDimensions);
    }

    let width = raster_width as u32;
    let height = raster_height as u32;
    let mut pixmap = Pixmap::new(width, height).ok_or(TextRasterError::AllocationFailed)?;
    if plan.background_enabled {
        let background = rounded_rect_path(
            shadow_left,
            shadow_top,
            base_width,
            base_height,
            plan.background_radius,
        )
        .ok_or(TextRasterError::InvalidPath)?;
        fill_path(
            &mut pixmap,
            &background,
            plan.background_color,
            Transform::identity(),
        );
    }

    if let Some(path) = path.as_ref() {
        let translate_x = shadow_left + outline_padding + background_padding_x - visual_left;
        let translate_y = shadow_top + outline_padding + background_padding_y - visual_top;
        if plan.shadow_enabled {
            let transform = Transform::from_translate(
                translate_x + plan.shadow_offset_x,
                translate_y + plan.shadow_offset_y,
            );
            if plan.outline_enabled && plan.outline_width > 0.0 {
                stroke_path(
                    &mut pixmap,
                    path,
                    plan.shadow_color,
                    plan.outline_width * 2.0,
                    transform,
                );
            }
            fill_path(&mut pixmap, path, plan.shadow_color, transform);
        }
        let transform = Transform::from_translate(translate_x, translate_y);
        if plan.outline_enabled && plan.outline_width > 0.0 {
            stroke_path(
                &mut pixmap,
                path,
                plan.outline_color,
                plan.outline_width * 2.0,
                transform,
            );
        }
        fill_path(&mut pixmap, path, plan.color, transform);
    }

    Ok(VideoFrame {
        width,
        height,
        rgba: straight_alpha_rgba(pixmap.data()),
        timestamp_seconds,
    })
}

/// One styled run reduced to what the layout needs.
struct RunSegment {
    text: String,
    color: RgbaColor,
    outline_color: RgbaColor,
    /// `<s...,outline>` width, when the run asked for its own.
    outline_width: Option<f32>,
    font_size: f32,
    letter_spacing: f32,
    x_scale: f32,
    y_scale: f32,
    rotation: f32,
    /// Line spacing this run requests for the gap after its line.
    line_spacing: Option<f32>,
    /// `<p...>` directive carried by the run.
    anchor: TextAnchor,
    /// Ruby block membership, if the run is inside `</>...`.
    ruby: Option<RubyRun>,
    /// Emoji image this run draws instead of a glyph.
    emoji: Option<String>,
    /// Index into the per-run faces passed to the layout.
    face_index: usize,
    strikethrough: bool,
}

/// One horizontal rule drawn through a run that asked for `<@...S>`.
/// One emoji image to draw, recorded during layout.
///
/// The cell keeps the image's own aspect ratio instead of a square: AviUtl
/// advances by the picture's proportions, so a wide emoji occupies more of the
/// line and a tall one less.
struct EmojiDraw {
    name: String,
    x: f32,
    baseline: f32,
    width: f32,
    height: f32,
}

/// Folders searched for `<&name>` images, in order.
static EMOJI_DIRS: OnceLock<Mutex<Vec<PathBuf>>> = OnceLock::new();
/// Decoded images keyed by path; a failed load is cached as `None`.
static EMOJI_CACHE: OnceLock<Mutex<HashMap<PathBuf, Option<Arc<Pixmap>>>>> = OnceLock::new();

/// Registers the folders `<&name>` looks in.
///
/// The interface calls this once at startup with the application data folder;
/// tests pass a temporary directory.
pub fn set_emoji_directories(directories: Vec<PathBuf>) {
    let slot = EMOJI_DIRS.get_or_init(|| Mutex::new(Vec::new()));
    if let Ok(mut dirs) = slot.lock() {
        *dirs = directories;
    }
}

/// Width divided by height, or 1.0 when the image is missing.
///
/// Bounded so a pathological image cannot push a line off the raster.
fn emoji_aspect(name: &str) -> f32 {
    const MIN_ASPECT: f32 = 0.05;
    const MAX_ASPECT: f32 = 20.0;
    let Some(source) = emoji_pixmap(name) else {
        return 1.0;
    };
    let (width, height) = (source.width() as f32, source.height() as f32);
    if width <= 0.0 || height <= 0.0 {
        return 1.0;
    }
    (width / height).clamp(MIN_ASPECT, MAX_ASPECT)
}

fn emoji_pixmap(name: &str) -> Option<Arc<Pixmap>> {
    let directories = EMOJI_DIRS
        .get()
        .and_then(|slot| slot.lock().ok().map(|dirs| dirs.clone()))
        .unwrap_or_default();
    for directory in directories {
        // AviUtl2 reads whatever image file sits in the folder, so the common
        // still formats are accepted instead of PNG alone.
        for extension in [
            "png", "PNG", "bmp", "BMP", "jpg", "JPG", "jpeg", "JPEG", "gif", "GIF",
        ] {
            let path = directory.join(format!("{name}.{extension}"));
            if !path.is_file() {
                continue;
            }
            let cache = EMOJI_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
            if let Ok(mut cache) = cache.lock() {
                if let Some(hit) = cache.get(&path) {
                    return hit.clone();
                }
                let pixmap = load_emoji(&path);
                cache.insert(path, pixmap.clone());
                return pixmap;
            }
        }
    }
    None
}

fn load_emoji(path: &std::path::Path) -> Option<Arc<Pixmap>> {
    let image = image::open(path).ok()?.to_rgba8();
    let (width, height) = image.dimensions();
    let mut data = Vec::with_capacity((width as usize) * (height as usize) * 4);
    for pixel in image.pixels() {
        let [red, green, blue, alpha] = pixel.0;
        let alpha = u16::from(alpha);
        // tiny-skia wants premultiplied colours.
        let premultiply = |channel: u8| ((u16::from(channel) * alpha + 127) / 255) as u8;
        data.extend_from_slice(&[
            premultiply(red),
            premultiply(green),
            premultiply(blue),
            alpha as u8,
        ]);
    }
    let size = tiny_skia::IntSize::from_wh(width, height)?;
    Pixmap::from_vec(data, size).map(Arc::new)
}

struct StrikeLine {
    x: f32,
    y: f32,
    width: f32,
    thickness: f32,
    color: RgbaColor,
}

struct ShapedSegment {
    /// Index of the run this segment came from, so ruby blocks can find their
    /// base span after layout.
    run_index: usize,
    glyphs: Vec<GlyphPlacement>,
    /// Glyph indices in `glyphs` that are spaces; each one is a word gap for
    /// justification.
    spaces: Vec<usize>,
    width: f32,
    ascent: f32,
    scale: f32,
    x_scale: f32,
    y_scale: f32,
    rotation: f32,
    color: RgbaColor,
    outline_color: RgbaColor,
    /// `<s...,outline>` width for this segment, when it asked for one.
    outline_width: Option<f32>,
    face_index: usize,
    font_size: f32,
    strikethrough: bool,
    line_spacing: Option<f32>,
    /// Emoji image this segment draws instead of glyphs.
    emoji: Option<String>,
}

struct ShapedRunLine {
    segments: Vec<ShapedSegment>,
    width: f32,
    anchor: Option<LineAnchor>,
}

/// Where an anchored line starts. `y` is explicit on the line a `<p...>`
/// opened and `None` on the lines that continue below it.
#[derive(Clone, Copy)]
struct LineAnchor {
    x: f32,
    y: Option<f32>,
}

/// A line being assembled, before its segments are shaped.
#[derive(Default)]
struct DraftLine {
    segments: Vec<(usize, String)>,
    anchor: Option<LineAnchor>,
}

/// The base text and furigana of one `</>...` block, shaped before layout.
#[derive(Default)]
struct RubyBlockShape {
    base_runs: Vec<usize>,
    ruby_runs: Vec<usize>,
    segments: Vec<ShapedSegment>,
    width: f32,
    ascent: f32,
    descent: f32,
    scale: f32,
    expand: bool,
}

const RUBY_GAP: f32 = 2.0;

fn collect_ruby_blocks(segments: &[RunSegment]) -> BTreeMap<u32, RubyBlockShape> {
    let mut blocks: BTreeMap<u32, RubyBlockShape> = BTreeMap::new();
    for (index, segment) in segments.iter().enumerate() {
        let Some(ruby) = segment.ruby else {
            continue;
        };
        let block = blocks.entry(ruby.block).or_insert_with(|| RubyBlockShape {
            scale: ruby.scale,
            expand: ruby.expand,
            ..RubyBlockShape::default()
        });
        if ruby.base {
            block.base_runs.push(index);
        } else {
            block.ruby_runs.push(index);
            block.scale = ruby.scale;
            block.expand = ruby.expand;
        }
    }
    // A block without both halves is a stray directive, not a ruby.
    blocks.retain(|_, block| !block.base_runs.is_empty() && !block.ruby_runs.is_empty());
    blocks
}

fn shape_ruby_block(block: &mut RubyBlockShape, segments: &[RunSegment], faces: &[Face<'_>]) {
    let mut width = 0.0_f32;
    let mut ascent = 0.0_f32;
    let mut descent = 0.0_f32;
    for &index in &block.ruby_runs {
        let segment = &segments[index];
        let face = &faces[segment.face_index];
        let units_per_em = face.units_per_em().max(1) as f32;
        let font_size = (segment.font_size * block.scale).max(1.0);
        let scale = font_size / units_per_em;
        let layout = shape_line(face, &segment.text, scale, segment.letter_spacing);
        let segment_ascent = (f32::from(face.ascender()) * scale).max(font_size * 0.5);
        let segment_descent = (-f32::from(face.descender()) * scale).max(font_size * 0.1);
        width += layout.width;
        ascent = ascent.max(segment_ascent);
        descent = descent.max(segment_descent);
        block.segments.push(ShapedSegment {
            run_index: index,
            emoji: None,
            glyphs: layout.glyphs,
            spaces: layout.spaces,
            width: layout.width,
            ascent: segment_ascent,
            scale,
            x_scale: segment.x_scale,
            y_scale: segment.y_scale,
            rotation: segment.rotation,
            color: segment.color,
            outline_color: segment.outline_color,
            outline_width: segment.outline_width,
            face_index: segment.face_index,
            font_size,
            strikethrough: false,
            line_spacing: None,
        });
    }
    block.width = width;
    block.ascent = ascent;
    block.descent = descent;
}

struct RunPath {
    path: Path,
    color: RgbaColor,
    outline_color: RgbaColor,
    /// `<s...,outline>` width; the plan's width applies when this is `None`.
    outline_width: Option<f32>,
}

/// Lays out and rasterizes text that carries more than one styled run.
///
/// AviUtl's control characters can change size, colour and font mid-line, so
/// each segment is shaped with its own face and scale and placed on a common
/// baseline; line metrics come from the tallest run so mixed sizes stay
/// aligned. Justification is left to the single-style path: spreading words
/// across runs with different advances needs a per-segment distribution that
/// this batch does not claim.
fn rasterize_runs(
    plan: &TextRenderPlan,
    timestamp_seconds: f64,
    faces: &[Face<'_>],
) -> Result<VideoFrame, TextRasterError> {
    if plan.runs.is_empty() {
        // A timing directive cleared every character: draw an empty frame
        // instead of a background or outline with nothing inside it.
        return Ok(VideoFrame {
            width: 1,
            height: 1,
            rgba: vec![0, 0, 0, 0],
            timestamp_seconds,
        });
    }
    let segments = plan
        .runs
        .iter()
        .enumerate()
        .map(|(index, run)| RunSegment {
            text: run.text.clone(),
            color: run.color,
            outline_color: run.outline_color,
            outline_width: run.outline_width,
            font_size: run.font_size.max(1.0),
            letter_spacing: run.letter_spacing,
            x_scale: run.x_scale.max(0.0),
            y_scale: run.y_scale.max(0.0),
            rotation: run.rotation,
            line_spacing: run.line_spacing,
            anchor: run.anchor,
            ruby: run.ruby,
            emoji: run.emoji.clone(),
            face_index: index,
            strikethrough: run.strikethrough,
        })
        .collect::<Vec<_>>();
    let mut ascent = 0.0_f32;
    let mut descent = 0.0_f32;
    let mut natural_advance = 0.0_f32;
    for segment in &segments {
        let face = &faces[segment.face_index];
        let units_per_em = face.units_per_em().max(1) as f32;
        let scale = segment.font_size / units_per_em;
        let segment_ascent = (f32::from(face.ascender()) * scale).max(segment.font_size * 0.5);
        let segment_descent = (-f32::from(face.descender()) * scale).max(segment.font_size * 0.1);
        let line_height = (segment_ascent + segment_descent).max(segment.font_size);
        ascent = ascent.max(segment_ascent);
        descent = descent.max(segment_descent);
        natural_advance = natural_advance
            .max((line_height + f32::from(face.line_gap()) * scale).max(line_height));
    }
    let mut ruby_blocks = collect_ruby_blocks(&segments);
    for block in ruby_blocks.values_mut() {
        shape_ruby_block(block, &segments, faces);
    }

    // Split every run on newlines, appending to the current logical line so a
    // directive can straddle one without moving the text that follows it.
    // Every draft line remembers an anchor when `<p...>` started it; a
    // continuation line inside an anchored block only remembers its x, so its
    // baseline can follow the previous line's advance.
    let mut drafts: Vec<DraftLine> = vec![DraftLine::default()];
    let mut anchor_x: Option<f32> = None;
    for (index, segment) in segments.iter().enumerate() {
        if segment.ruby.is_some_and(|ruby| !ruby.base) {
            // Furigana is positioned over its base text after layout.
            continue;
        }
        if !matches!(segment.anchor, TextAnchor::Flow) {
            if !drafts.last().is_some_and(|line| line.segments.is_empty()) {
                drafts.push(DraftLine::default());
            }
            let line = drafts.last_mut().expect("a draft line exists");
            match segment.anchor {
                TextAnchor::At { x, y } => {
                    line.anchor = Some(LineAnchor { x, y: Some(y) });
                    anchor_x = Some(x);
                }
                TextAnchor::Reset => {
                    line.anchor = None;
                    anchor_x = None;
                }
                TextAnchor::Flow => {}
            }
        }
        let mut parts = segment.text.split('\n');
        let first = parts.next().unwrap_or_default();
        if !first.is_empty() {
            drafts
                .last_mut()
                .expect("at least one line")
                .segments
                .push((index, first.to_owned()));
        }
        for part in parts {
            drafts.push(DraftLine {
                segments: Vec::new(),
                anchor: anchor_x.map(|x| LineAnchor { x, y: None }),
            });
            if !part.is_empty() {
                drafts
                    .last_mut()
                    .expect("a line was just pushed")
                    .segments
                    .push((index, part.to_owned()));
            }
        }
    }

    let mut lines = Vec::with_capacity(drafts.len());
    for draft in drafts {
        let mut shaped_segments = Vec::with_capacity(draft.segments.len());
        let mut width = 0.0_f32;
        for (index, text) in draft.segments {
            let segment = &segments[index];
            if let Some(emoji) = &segment.emoji {
                // The cell keeps the image's proportions; the caller draws it.
                let cell_width = segment.font_size * emoji_aspect(emoji);
                shaped_segments.push(ShapedSegment {
                    run_index: index,
                    glyphs: Vec::new(),
                    spaces: Vec::new(),
                    width: cell_width,
                    ascent: segment.font_size * 0.8,
                    scale: 1.0,
                    x_scale: 1.0,
                    y_scale: 1.0,
                    rotation: 0.0,
                    color: segment.color,
                    outline_color: segment.outline_color,
                    outline_width: segment.outline_width,
                    face_index: segment.face_index,
                    font_size: segment.font_size,
                    strikethrough: false,
                    line_spacing: segment.line_spacing,
                    emoji: Some(emoji.clone()),
                });
                width += cell_width;
                continue;
            }
            let face = &faces[segment.face_index];
            let units_per_em = face.units_per_em().max(1) as f32;
            let scale = segment.font_size / units_per_em;
            let layout = shape_line(face, &text, scale, segment.letter_spacing);
            let layout = GlyphScale {
                x: segment.x_scale,
                y: segment.y_scale,
            }
            .apply(layout);
            width += layout.width;
            shaped_segments.push(ShapedSegment {
                run_index: segment.face_index,
                glyphs: layout.glyphs,
                spaces: layout.spaces,
                width: layout.width,
                ascent: (f32::from(face.ascender()) * scale).max(segment.font_size * 0.5),
                scale,
                x_scale: segment.x_scale,
                y_scale: segment.y_scale,
                rotation: segment.rotation,
                color: segment.color,
                outline_color: segment.outline_color,
                outline_width: segment.outline_width,
                face_index: segment.face_index,
                font_size: segment.font_size,
                strikethrough: segment.strikethrough,
                line_spacing: segment.line_spacing,
                emoji: None,
            });
        }
        lines.push(ShapedRunLine {
            segments: shaped_segments,
            width,
            anchor: draft.anchor,
        });
    }

    let object_advance = if plan.line_spacing > 0.0 {
        plan.line_spacing
    } else {
        natural_advance
    };
    let advance_after = |line: &ShapedRunLine| {
        line.segments
            .iter()
            .rev()
            .find_map(|segment| segment.line_spacing)
            .filter(|spacing| *spacing > 0.0)
            .unwrap_or(object_advance)
    };
    let normal_width = lines
        .iter()
        .filter(|line| line.anchor.is_none())
        .map(|line| line.width)
        .fold(0.0_f32, f32::max);
    // AviUtl justifies every line of a paragraph except its last one, which is
    // the rule the single-style path already follows.
    if plan.alignment == TextAlignment::Justify {
        justify_run_lines(&mut lines, normal_width);
    }
    // Which line each base run landed on, and how tall the tallest base glyph
    // on that line is, decide where furigana sits and how much the previous
    // line has to grow to make room for it.
    let mut run_line: HashMap<usize, usize> = HashMap::new();
    let mut line_base_ascent = vec![0.0_f32; lines.len()];
    for (line_index, line) in lines.iter().enumerate() {
        for segment in &line.segments {
            run_line.insert(segment.run_index, line_index);
            line_base_ascent[line_index] = line_base_ascent[line_index].max(segment.ascent);
        }
    }
    let mut line_ruby_extra = vec![0.0_f32; lines.len()];
    for block in ruby_blocks.values() {
        if !block.expand {
            continue;
        }
        let Some(&line_index) = block.base_runs.iter().find_map(|run| run_line.get(run)) else {
            continue;
        };
        let extra = block.ascent + block.descent + RUBY_GAP;
        line_ruby_extra[line_index] = line_ruby_extra[line_index].max(extra);
    }
    // Normal lines flow below each other; anchored lines start where `<p...>`
    // put them and continue downwards at the same x.
    let mut positions = Vec::with_capacity(lines.len());
    let mut normal_baseline = ascent;
    let mut anchor_baseline: Option<f32> = None;
    for (line_index, line) in lines.iter().enumerate() {
        let advance =
            advance_after(line) + line_ruby_extra.get(line_index + 1).copied().unwrap_or(0.0);
        match line.anchor {
            None => {
                positions.push((
                    alignment_offset(plan.alignment, normal_width, line.width),
                    normal_baseline,
                ));
                normal_baseline += advance;
                anchor_baseline = None;
            }
            Some(LineAnchor { x, y: Some(y) }) => {
                let baseline = y + ascent;
                positions.push((x, baseline));
                anchor_baseline = Some(baseline + advance);
            }
            Some(LineAnchor { x, y: None }) => {
                let baseline = anchor_baseline.unwrap_or(ascent);
                positions.push((x, baseline));
                anchor_baseline = Some(baseline + advance);
            }
        }
    }
    let logical_width = lines
        .iter()
        .zip(&positions)
        .map(|(line, (x, _))| x + line.width)
        .fold(normal_width, f32::max);
    let logical_height = positions
        .iter()
        .map(|(_, baseline)| baseline + descent)
        .fold(ascent, f32::max);
    if !logical_width.is_finite() || !logical_height.is_finite() {
        return Err(TextRasterError::InvalidDimensions);
    }

    let mut placed: Vec<Option<(usize, f32, f32)>> = vec![None; segments.len()];
    for (line_index, line) in lines.iter().enumerate() {
        let (line_x, _) = positions[line_index];
        let mut segment_x = line_x;
        for segment in &line.segments {
            placed[segment.run_index] = Some((line_index, segment_x, segment.width));
            segment_x += segment.width;
        }
    }

    let mut paths = Vec::new();
    let mut strikes = Vec::new();
    let mut emojis: Vec<EmojiDraw> = Vec::new();
    let mut outlined_glyphs = 0_usize;
    for (line_index, line) in lines.iter().enumerate() {
        let (line_x, baseline) = positions[line_index];
        let mut segment_x = line_x;
        for segment in &line.segments {
            if let Some(emoji) = &segment.emoji {
                emojis.push(EmojiDraw {
                    name: emoji.clone(),
                    x: segment_x,
                    baseline,
                    width: segment.width,
                    height: segment.font_size,
                });
                segment_x += segment.width;
                continue;
            }
            let face = &faces[segment.face_index];
            let mut builder = PathBuilder::new();
            for glyph in &segment.glyphs {
                let radians = segment.rotation.to_radians();
                let mut outline = GlyphOutline {
                    path: &mut builder,
                    origin_x: segment_x + glyph.x,
                    baseline_y: baseline - glyph.y_offset,
                    scale: segment.scale,
                    x_scale: segment.x_scale,
                    y_scale: segment.y_scale,
                    pivot_x: segment_x,
                    pivot_y: baseline,
                    cos: radians.cos(),
                    sin: radians.sin(),
                };
                if face
                    .outline_glyph(GlyphId(glyph.glyph_id), &mut outline)
                    .is_some()
                {
                    outlined_glyphs += 1;
                }
            }
            if let Some(path) = builder.finish() {
                paths.push(RunPath {
                    path,
                    color: segment.color,
                    outline_color: segment.outline_color,
                    outline_width: segment.outline_width,
                });
            }
            if segment.strikethrough && segment.rotation == 0.0 && segment.width > 0.0 {
                strikes.push(StrikeLine {
                    x: segment_x,
                    y: baseline - segment.font_size * 0.32,
                    width: segment.width,
                    thickness: (segment.font_size * 0.06).max(1.0),
                    color: segment.color,
                });
            }
            segment_x += segment.width;
        }
    }
    // Furigana is drawn over its base span, centred and a little above the
    // tallest base glyph on that line.
    for block in ruby_blocks.values() {
        let Some(line_index) = block
            .base_runs
            .iter()
            .find_map(|run| run_line.get(run).copied())
        else {
            continue;
        };
        let mut start = f32::INFINITY;
        let mut end = f32::NEG_INFINITY;
        for run in &block.base_runs {
            if let Some((_, x, width)) = placed[*run] {
                start = start.min(x);
                end = end.max(x + width);
            }
        }
        if !start.is_finite() || !end.is_finite() {
            continue;
        }
        let ruby_x = start + (end - start - block.width).max(0.0) / 2.0;
        let (_, baseline) = positions[line_index];
        let ruby_baseline = baseline - line_base_ascent[line_index] - RUBY_GAP - block.descent;
        let mut segment_x = ruby_x;
        for segment in &block.segments {
            let face = &faces[segment.face_index];
            let radians = segment.rotation.to_radians();
            let mut builder = PathBuilder::new();
            for glyph in &segment.glyphs {
                let mut outline = GlyphOutline {
                    path: &mut builder,
                    origin_x: segment_x + glyph.x,
                    baseline_y: ruby_baseline - glyph.y_offset,
                    scale: segment.scale,
                    x_scale: segment.x_scale,
                    y_scale: segment.y_scale,
                    pivot_x: segment_x,
                    pivot_y: ruby_baseline,
                    cos: radians.cos(),
                    sin: radians.sin(),
                };
                if face
                    .outline_glyph(GlyphId(glyph.glyph_id), &mut outline)
                    .is_some()
                {
                    outlined_glyphs += 1;
                }
            }
            if let Some(path) = builder.finish() {
                paths.push(RunPath {
                    path,
                    color: segment.color,
                    outline_color: segment.outline_color,
                    outline_width: segment.outline_width,
                });
            }
            segment_x += segment.width;
        }
    }
    let has_visible_text = segments.iter().any(|segment| {
        segment
            .text
            .chars()
            .any(|character| !ignorable_for_font_matching(character))
    });
    if has_visible_text && outlined_glyphs == 0 && emojis.is_empty() {
        return Err(TextRasterError::InvalidPath);
    }

    let mut visual_left = 0.0_f32;
    let mut visual_top = 0.0_f32;
    let mut visual_right = logical_width;
    let mut visual_bottom = logical_height;
    for run_path in &paths {
        let bounds = run_path.path.bounds();
        visual_left = visual_left.min(bounds.left());
        visual_top = visual_top.min(bounds.top());
        visual_right = visual_right.max(bounds.right());
        visual_bottom = visual_bottom.max(bounds.bottom());
    }
    for emoji in &emojis {
        visual_left = visual_left.min(emoji.x);
        visual_top = visual_top.min(emoji.baseline - emoji.height);
        visual_right = visual_right.max(emoji.x + emoji.width);
        visual_bottom = visual_bottom.max(emoji.baseline);
    }
    let visual_width = (visual_right - visual_left).max(0.0);
    let visual_height = (visual_bottom - visual_top).max(0.0);
    // A run with its own outline can ask for more room than the object's.
    let outline_padding = if plan.outline_enabled {
        run_outline_width(plan, paths.iter().map(|run| run.outline_width)).ceil() + 2.0
    } else {
        2.0
    };
    let background_padding_x = if plan.background_enabled {
        plan.background_padding_x
    } else {
        0.0
    };
    let background_padding_y = if plan.background_enabled {
        plan.background_padding_y
    } else {
        0.0
    };
    let base_width = visual_width + (outline_padding + background_padding_x) * 2.0;
    let base_height = visual_height + (outline_padding + background_padding_y) * 2.0;
    let shadow_left = if plan.shadow_enabled {
        (-plan.shadow_offset_x).max(0.0)
    } else {
        0.0
    };
    let shadow_right = if plan.shadow_enabled {
        plan.shadow_offset_x.max(0.0)
    } else {
        0.0
    };
    let shadow_top = if plan.shadow_enabled {
        (-plan.shadow_offset_y).max(0.0)
    } else {
        0.0
    };
    let shadow_bottom = if plan.shadow_enabled {
        plan.shadow_offset_y.max(0.0)
    } else {
        0.0
    };
    let raster_width = (base_width + shadow_left + shadow_right).ceil().max(1.0);
    let raster_height = (base_height + shadow_top + shadow_bottom).ceil().max(1.0);
    if !raster_width.is_finite()
        || !raster_height.is_finite()
        || raster_width > MAX_RASTER_DIMENSION
        || raster_height > MAX_RASTER_DIMENSION
    {
        return Err(TextRasterError::InvalidDimensions);
    }

    let width = raster_width as u32;
    let height = raster_height as u32;
    let mut pixmap = Pixmap::new(width, height).ok_or(TextRasterError::AllocationFailed)?;
    if plan.background_enabled {
        let background = rounded_rect_path(
            shadow_left,
            shadow_top,
            base_width,
            base_height,
            plan.background_radius,
        )
        .ok_or(TextRasterError::InvalidPath)?;
        fill_path(
            &mut pixmap,
            &background,
            plan.background_color,
            Transform::identity(),
        );
    }

    let translate_x = shadow_left + outline_padding + background_padding_x - visual_left;
    let translate_y = shadow_top + outline_padding + background_padding_y - visual_top;
    for run_path in &paths {
        let width = if plan.outline_enabled {
            run_path.outline_width.unwrap_or(plan.outline_width)
        } else {
            0.0
        };
        if plan.shadow_enabled {
            let transform = Transform::from_translate(
                translate_x + plan.shadow_offset_x,
                translate_y + plan.shadow_offset_y,
            );
            if width > 0.0 {
                stroke_path(
                    &mut pixmap,
                    &run_path.path,
                    plan.shadow_color,
                    width * 2.0,
                    transform,
                );
            }
            fill_path(&mut pixmap, &run_path.path, plan.shadow_color, transform);
        }
        let transform = Transform::from_translate(translate_x, translate_y);
        if width > 0.0 {
            stroke_path(
                &mut pixmap,
                &run_path.path,
                run_path.outline_color,
                width * 2.0,
                transform,
            );
        }
        fill_path(&mut pixmap, &run_path.path, run_path.color, transform);
    }
    for emoji in &emojis {
        let Some(source) = emoji_pixmap(&emoji.name) else {
            continue;
        };
        let (width, height) = (source.width() as f32, source.height() as f32);
        if width <= 0.0 || height <= 0.0 {
            continue;
        }
        // The cell already has the image's proportions, so one scale fits both
        // axes and the picture fills it instead of being letterboxed.
        let scale = emoji.height / height;
        let draw_x = emoji.x;
        let draw_y = emoji.baseline - emoji.height;
        let transform = Transform::from_translate(draw_x + translate_x, draw_y + translate_y)
            .pre_scale(scale, scale);
        pixmap.draw_pixmap(
            0,
            0,
            source.as_ref().as_ref(),
            &PixmapPaint::default(),
            transform,
            None,
        );
    }
    for strike in &strikes {
        if let Some(rect) = tiny_skia::Rect::from_xywh(
            strike.x + translate_x,
            strike.y + translate_y,
            strike.width,
            strike.thickness,
        ) {
            let mut builder = PathBuilder::new();
            builder.push_rect(rect);
            if let Some(path) = builder.finish() {
                fill_path(&mut pixmap, &path, strike.color, Transform::identity());
            }
        }
    }

    Ok(VideoFrame {
        width,
        height,
        rgba: straight_alpha_rgba(pixmap.data()),
        timestamp_seconds,
    })
}

fn shape_line(face: &Face<'_>, text: &str, scale: f32, letter_spacing: f32) -> LineLayout {
    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    let glyphs = rustybuzz::shape(face, &[], buffer);
    let infos = glyphs.glyph_infos();
    let positions = glyphs.glyph_positions();
    let total_advance = positions
        .iter()
        .map(|position| position.x_advance as f32 * scale)
        .sum::<f32>();
    let spacing_direction = if total_advance < 0.0 { -1.0 } else { 1.0 };
    let mut pen_x = 0.0_f32;
    let mut minimum = 0.0_f32;
    let mut maximum = 0.0_f32;
    let mut placements = Vec::with_capacity(infos.len());
    let mut spaces = Vec::new();
    for (index, (info, position)) in infos.iter().zip(positions).enumerate() {
        if text
            .as_bytes()
            .get(info.cluster as usize)
            .is_some_and(|byte| *byte == b' ')
        {
            spaces.push(index);
        }
        placements.push(GlyphPlacement {
            glyph_id: info.glyph_id as u16,
            x: pen_x + position.x_offset as f32 * scale,
            y_offset: position.y_offset as f32 * scale,
        });
        pen_x += position.x_advance as f32 * scale;
        if index + 1 < infos.len() {
            pen_x += letter_spacing * spacing_direction;
        }
        minimum = minimum.min(pen_x);
        maximum = maximum.max(pen_x);
    }
    for glyph in &mut placements {
        glyph.x -= minimum;
    }
    LineLayout {
        glyphs: placements,
        width: (maximum - minimum).max(0.0),
        spaces,
    }
}

/// Repositions the glyphs of one line so its words spread across
/// `target_width`, leaving the outlines untouched. Returns `None` when there is
/// nothing to justify: a single word, or a line that already fills the width.
fn justified_glyphs(
    face: &Face<'_>,
    text: &str,
    scale: f32,
    letter_spacing: f32,
    target_width: f32,
) -> Option<Vec<GlyphPlacement>> {
    let word_layouts = text
        .split(' ')
        .map(|word| shape_line(face, word, scale, letter_spacing))
        .collect::<Vec<_>>();
    let widths = word_layouts
        .iter()
        .map(|layout| layout.width)
        .collect::<Vec<_>>();
    let space_width = shape_line(face, " ", scale, 0.0).width;
    let offsets = justified_word_offsets(&widths, space_width, target_width)?;
    let mut glyphs = Vec::new();
    for (layout, offset) in word_layouts.iter().zip(offsets) {
        for glyph in &layout.glyphs {
            glyphs.push(GlyphPlacement {
                glyph_id: glyph.glyph_id,
                x: offset + glyph.x,
                y_offset: glyph.y_offset,
            });
        }
    }
    // The justified positions are offsets into this rebuilt glyph list, which
    // the single-face path does not use.
    Some(glyphs)
}

/// Distributes the width a justified line has left over across its word gaps.
/// The offsets are the x position of each word; `None` means the line cannot be
/// justified and should stay left-aligned.
fn justified_word_offsets(
    word_widths: &[f32],
    space_width: f32,
    target_width: f32,
) -> Option<Vec<f32>> {
    let gaps = word_widths.len().checked_sub(1)?;
    if gaps == 0 || !target_width.is_finite() {
        return None;
    }
    let natural = word_widths.iter().sum::<f32>() + space_width * gaps as f32;
    if !natural.is_finite() || target_width <= natural {
        return None;
    }
    let extra_per_gap = (target_width - natural) / gaps as f32;
    let mut offsets = Vec::with_capacity(word_widths.len());
    let mut x = 0.0_f32;
    for (index, width) in word_widths.iter().enumerate() {
        offsets.push(x);
        x += width;
        if index + 1 < word_widths.len() {
            x += space_width + extra_per_gap;
        }
    }
    Some(offsets)
}

/// Justifies every line of a run block except its last one, which keeps its
/// natural width the way the single-style path leaves a paragraph's last line.
fn justify_run_lines(lines: &mut [ShapedRunLine], target_width: f32) {
    let last = lines.len().saturating_sub(1);
    for line in &mut lines[..last] {
        justify_run_line(line, target_width);
    }
}

/// Spreads a justified run line's spare width across its word gaps.
///
/// A gap can sit inside a styled run or on a boundary between runs, so each
/// segment's glyphs are shifted by the advance collected within it and the
/// segment's width grows by the gaps it contains. That keeps the following
/// runs, strikethroughs, and ruby spans in step with the wider line. A space
/// only counts as a gap when text follows it on the line: a trailing space
/// would otherwise take a share of the spare width and stretch the line's
/// strikethrough past the text it belongs to.
fn justify_run_line(line: &mut ShapedRunLine, target_width: f32) -> bool {
    if line.anchor.is_some() {
        return false;
    }
    // The last glyph on the line that is not a space, or the last emoji cell.
    // Any space after it is trailing and cannot be a word gap.
    let mut last_text = None;
    for (segment_index, segment) in line.segments.iter().enumerate() {
        if segment.emoji.is_some() {
            // An emoji is content even though it carries no glyphs, so a space
            // before it is a real word gap.
            last_text = Some((segment_index, usize::MAX));
            continue;
        }
        for (glyph_index, _) in segment.glyphs.iter().enumerate() {
            if segment.spaces.binary_search(&glyph_index).is_err() {
                last_text = Some((segment_index, glyph_index));
            }
        }
    }
    let is_gap = |segment_index: usize, glyph_index: usize| {
        last_text.is_some_and(|last| (segment_index, glyph_index) < last)
    };
    let gaps = line
        .segments
        .iter()
        .enumerate()
        .map(|(segment_index, segment)| {
            segment
                .spaces
                .iter()
                .filter(|&&glyph_index| is_gap(segment_index, glyph_index))
                .count()
        })
        .sum::<usize>();
    if gaps == 0 || !target_width.is_finite() || target_width <= line.width {
        return false;
    }
    let extra_per_gap = (target_width - line.width) / gaps as f32;
    let mut shift = 0.0_f32;
    for (segment_index, segment) in line.segments.iter_mut().enumerate() {
        let start_shift = shift;
        let mut spaces = segment
            .spaces
            .iter()
            .copied()
            .filter(|glyph_index| is_gap(segment_index, *glyph_index))
            .peekable();
        for (index, glyph) in segment.glyphs.iter_mut().enumerate() {
            glyph.x += shift - start_shift;
            if spaces.peek() == Some(&index) {
                shift += extra_per_gap;
                spaces.next();
            }
        }
        segment.width += shift - start_shift;
    }
    line.width = target_width;
    true
}

fn alignment_offset(alignment: TextAlignment, width: f32, line_width: f32) -> f32 {
    match alignment {
        TextAlignment::Left | TextAlignment::Justify => 0.0,
        TextAlignment::Right => (width - line_width).max(0.0),
        TextAlignment::Center => ((width - line_width) / 2.0).max(0.0),
    }
}

struct GlyphOutline<'a> {
    path: &'a mut PathBuilder,
    origin_x: f32,
    baseline_y: f32,
    scale: f32,
    x_scale: f32,
    y_scale: f32,
    /// Rotation happens on screen coordinates around the run's start point.
    pivot_x: f32,
    pivot_y: f32,
    cos: f32,
    sin: f32,
}

impl GlyphOutline<'_> {
    fn point(&self, x: f32, y: f32) -> (f32, f32) {
        let px = self.origin_x + x * self.x_scale * self.scale;
        let py = self.baseline_y - y * self.y_scale * self.scale;
        if self.sin == 0.0 && self.cos == 1.0 {
            return (px, py);
        }
        let dx = px - self.pivot_x;
        let dy = py - self.pivot_y;
        (
            self.pivot_x + dx * self.cos - dy * self.sin,
            self.pivot_y + dx * self.sin + dy * self.cos,
        )
    }
}

/// Applies `<tw...>` / `<th...>` to a shaped line.
struct GlyphScale {
    x: f32,
    y: f32,
}

impl GlyphScale {
    fn apply(&self, layout: LineLayout) -> LineLayout {
        if self.x == 1.0 && self.y == 1.0 {
            return layout;
        }
        LineLayout {
            glyphs: layout
                .glyphs
                .into_iter()
                .map(|glyph| GlyphPlacement {
                    glyph_id: glyph.glyph_id,
                    x: glyph.x * self.x,
                    y_offset: glyph.y_offset * self.y,
                })
                .collect(),
            width: layout.width * self.x,
            spaces: layout.spaces,
        }
    }
}

impl OutlineBuilder for GlyphOutline<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.point(x, y);
        self.path.move_to(x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.point(x, y);
        self.path.line_to(x, y);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (x1, y1) = self.point(x1, y1);
        let (x, y) = self.point(x, y);
        self.path.quad_to(x1, y1, x, y);
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (x1, y1) = self.point(x1, y1);
        let (x2, y2) = self.point(x2, y2);
        let (x, y) = self.point(x, y);
        self.path.cubic_to(x1, y1, x2, y2, x, y);
    }

    fn close(&mut self) {
        self.path.close();
    }
}

fn rounded_rect_path(x: f32, y: f32, width: f32, height: f32, radius: f32) -> Option<Path> {
    let rect = Rect::from_xywh(x, y, width, height)?;
    let radius = radius.min(width / 2.0).min(height / 2.0);
    if radius <= 0.0 {
        return Some(PathBuilder::from_rect(rect));
    }
    let right = x + width;
    let bottom = y + height;
    let mut builder = PathBuilder::new();
    builder.move_to(x + radius, y);
    builder.line_to(right - radius, y);
    builder.quad_to(right, y, right, y + radius);
    builder.line_to(right, bottom - radius);
    builder.quad_to(right, bottom, right - radius, bottom);
    builder.line_to(x + radius, bottom);
    builder.quad_to(x, bottom, x, bottom - radius);
    builder.line_to(x, y + radius);
    builder.quad_to(x, y, x + radius, y);
    builder.close();
    builder.finish()
}

fn fill_path(pixmap: &mut Pixmap, path: &Path, color: RgbaColor, transform: Transform) {
    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    paint.set_color(to_skia_color(color));
    pixmap.fill_path(path, &paint, FillRule::Winding, transform, None);
}

fn stroke_path(
    pixmap: &mut Pixmap,
    path: &Path,
    color: RgbaColor,
    width: f32,
    transform: Transform,
) {
    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    paint.set_color(to_skia_color(color));
    let stroke = Stroke {
        width,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    pixmap.stroke_path(path, &paint, &stroke, transform, None);
}

fn to_skia_color(color: RgbaColor) -> Color {
    Color::from_rgba8(color.red, color.green, color.blue, color.alpha)
}

fn straight_alpha_rgba(premultiplied: &[u8]) -> Vec<u8> {
    let mut straight = premultiplied.to_vec();
    for pixel in straight.as_chunks_mut::<4>().0 {
        let alpha = u16::from(pixel[3]);
        if alpha == 0 {
            pixel[..3].fill(0);
            continue;
        }
        for channel in &mut pixel[..3] {
            *channel = ((u16::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
        }
    }
    straight
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> TextRenderPlan {
        TextRenderPlan {
            content: "\u{ea01}\n\u{ea02}".to_owned(),
            runs: Vec::new(),
            runs_authoritative: false,
            font_family: "remixicon".to_owned(),
            font_size: 64.0,
            bold: false,
            italic: false,
            letter_spacing: 2.0,
            line_spacing: 72.0,
            alignment: TextAlignment::Right,
            color: RgbaColor {
                red: 240,
                green: 180,
                blue: 80,
                alpha: 192,
            },
            outline_enabled: true,
            outline_color: RgbaColor {
                red: 20,
                green: 40,
                blue: 60,
                alpha: 255,
            },
            outline_width: 3.0,
            shadow_enabled: true,
            shadow_color: RgbaColor {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 128,
            },
            shadow_offset_x: -6.0,
            shadow_offset_y: 5.0,
            background_enabled: true,
            background_color: RgbaColor {
                red: 30,
                green: 50,
                blue: 70,
                alpha: 160,
            },
            background_radius: 8.0,
            background_padding_x: 12.0,
            background_padding_y: 9.0,
            opacity: 1.0,
        }
    }

    fn run(text: &str, font_size: f32) -> freya_nle_core::api::TextRun {
        freya_nle_core::api::TextRun {
            text: text.to_owned(),
            color: RgbaColor {
                red: 255,
                green: 255,
                blue: 255,
                alpha: 255,
            },
            outline_color: RgbaColor {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 255,
            },
            font_size,
            letter_spacing: 0.0,
            line_spacing: None,
            anchor: TextAnchor::Flow,
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
        }
    }

    fn plain_plan(runs: Vec<freya_nle_core::api::TextRun>) -> TextRenderPlan {
        let mut plan = plan();
        plan.font_family = "remixicon".to_owned();
        plan.alignment = TextAlignment::Left;
        plan.outline_enabled = false;
        plan.shadow_enabled = false;
        plan.background_enabled = false;
        plan.line_spacing = 0.0;
        plan.content = runs.iter().map(|run| run.text.as_str()).collect();
        plan.runs = runs;
        plan.runs_authoritative = true;
        plan
    }

    fn opaque_pixels(frame: &VideoFrame) -> usize {
        frame
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| pixel[3] > 0)
            .count()
    }

    /// One shared emoji folder holding every test image.
    ///
    /// The folder list is a process-wide static, so tests that run in parallel
    /// must agree on a single directory instead of each replacing it; otherwise
    /// one test's registration hides the other's image.
    fn emoji_test_directory() -> std::path::PathBuf {
        let directory =
            std::env::temp_dir().join(format!("freya-nle-emoji-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("emoji directory");
        for (name, width, height, color) in [
            ("__test_star", 8u32, 8u32, [255u8, 0, 0, 255]),
            ("__test_wide", 32, 8, [0, 255, 0, 255]),
            ("__test_tall", 8, 32, [0, 0, 255, 255]),
        ] {
            let mut pixels = vec![0_u8; (width * height * 4) as usize];
            for pixel in pixels.chunks_exact_mut(4) {
                pixel.copy_from_slice(&color);
            }
            image::save_buffer(
                directory.join(format!("{name}.png")),
                &pixels,
                width,
                height,
                image::ColorType::Rgba8,
            )
            .expect("write png");
        }
        set_emoji_directories(vec![directory.clone()]);
        directory
    }

    #[test]
    fn an_emoji_run_draws_the_registered_image() {
        emoji_test_directory();

        let mut emoji = run("\u{FFFC}", 48.0);
        emoji.emoji = Some("__test_star".to_owned());
        let plan = plain_plan(vec![emoji]);
        let frame = test_rasterizer()
            .rasterize(&plan, 0.0)
            .expect("an emoji-only text rasterizes");
        assert!(
            frame
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[0] > 200 && pixel[1] < 60 && pixel[2] < 60 && pixel[3] > 0),
            "the registered image is drawn into the cell"
        );
    }

    #[test]
    fn an_emoji_may_be_saved_as_bmp_as_well_as_png() {
        let directory = emoji_test_directory();
        // AviUtl2 reads whatever image file the folder holds, so a BMP has to
        // resolve through the same lookup.
        let mut pixels = vec![0_u8; 8 * 8 * 4];
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[0, 128, 255, 255]);
        }
        image::save_buffer(
            directory.join("__test_bmp.bmp"),
            &pixels,
            8,
            8,
            image::ColorType::Rgba8,
        )
        .expect("write bmp");
        let mut emoji = run("\u{FFFC}", 48.0);
        emoji.emoji = Some("__test_bmp".to_owned());
        let frame = test_rasterizer()
            .rasterize(&plain_plan(vec![emoji]), 0.0)
            .expect("a BMP emoji rasterizes");
        assert!(
            frame
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[1] > 90 && pixel[2] > 200 && pixel[3] > 0),
            "the BMP image is drawn"
        );
    }

    #[test]
    fn an_emoji_cell_follows_the_image_aspect_ratio() {
        // `__test_wide` is 32x8 and `__test_tall` is 8x32.
        emoji_test_directory();

        let wide = {
            let mut emoji = run("\u{FFFC}", 48.0);
            emoji.emoji = Some("__test_wide".to_owned());
            plain_plan(vec![emoji])
        };
        let tall = {
            let mut emoji = run("\u{FFFC}", 48.0);
            emoji.emoji = Some("__test_tall".to_owned());
            plain_plan(vec![emoji])
        };
        let square = {
            let mut emoji = run("\u{FFFC}", 48.0);
            emoji.emoji = Some("__test_missing".to_owned());
            plain_plan(vec![emoji])
        };
        let rasterizer = test_rasterizer();
        let wide_frame = rasterizer.rasterize(&wide, 0.0).expect("wide emoji");
        let tall_frame = rasterizer.rasterize(&tall, 0.0).expect("tall emoji");
        let square_frame = rasterizer.rasterize(&square, 0.0).expect("fallback cell");
        // The wide picture advances four times the font size, the tall one
        // keeps the font size width; a missing image falls back to a square.
        assert!(
            wide_frame.width > tall_frame.width,
            "a wide emoji occupies more of the line ({} vs {})",
            wide_frame.width,
            tall_frame.width
        );
        // A missing image falls back to a square cell, which sits between the
        // wide and the tall one.
        assert!(
            wide_frame.width > square_frame.width && square_frame.width > tall_frame.width,
            "square cell between the two ({} > {} > {})",
            wide_frame.width,
            square_frame.width,
            tall_frame.width
        );
    }

    #[test]
    fn furigana_is_drawn_above_the_base_text() {
        use freya_nle_core::api::RubyRun;

        let rasterizer = test_rasterizer();
        let base = plain_plan(vec![run("\u{ea01}", 48.0)]);
        let mut base_run = run("\u{ea01}", 48.0);
        base_run.ruby = Some(RubyRun {
            block: 1,
            base: true,
            scale: 0.5,
            expand: false,
        });
        let mut ruby_run = run("\u{ea02}", 48.0);
        ruby_run.ruby = Some(RubyRun {
            block: 1,
            base: false,
            scale: 0.5,
            expand: false,
        });
        let with_ruby = plain_plan(vec![base_run, ruby_run]);
        let base_frame = rasterizer.rasterize(&base, 0.0).expect("base frame");
        let ruby_frame = rasterizer.rasterize(&with_ruby, 0.0).expect("ruby frame");
        assert!(
            ruby_frame.height > base_frame.height,
            "the furigana extends the frame above the base glyph"
        );

        // `<!+>` grows the line so the next one clears the furigana.
        let mut expanded_base = run("\u{ea01}", 48.0);
        expanded_base.ruby = Some(RubyRun {
            block: 2,
            base: true,
            scale: 0.5,
            expand: true,
        });
        let mut expanded_ruby = run("\u{ea02}", 48.0);
        expanded_ruby.ruby = Some(RubyRun {
            block: 2,
            base: false,
            scale: 0.5,
            expand: true,
        });
        let expanded = plain_plan(vec![expanded_base, expanded_ruby]);
        let expanded_frame = rasterizer
            .rasterize(&expanded, 0.0)
            .expect("expanded frame");
        assert!(expanded_frame.height >= ruby_frame.height);
    }

    #[test]
    fn a_rotated_run_turns_the_text_on_its_side() {
        let rasterizer = test_rasterizer();
        let plain = plain_plan(vec![run("\u{ea01}\u{ea02}", 48.0)]);
        let mut rotated_run = run("\u{ea01}\u{ea02}", 48.0);
        rotated_run.rotation = 90.0;
        let rotated = plain_plan(vec![rotated_run]);
        let plain_frame = rasterizer.rasterize(&plain, 0.0).expect("plain frame");
        let rotated_frame = rasterizer.rasterize(&rotated, 0.0).expect("rotated frame");
        assert!(
            plain_frame.width > plain_frame.height,
            "the unrotated run is wider than it is tall"
        );
        assert!(
            rotated_frame.height > rotated_frame.width,
            "rotating by 90 degrees turns that proportion around"
        );
    }

    #[test]
    fn a_horizontally_scaled_run_is_narrower() {
        let rasterizer = test_rasterizer();
        let plain = plain_plan(vec![run("\u{ea01}\u{ea02}", 48.0)]);
        let mut scaled_run = run("\u{ea01}\u{ea02}", 48.0);
        scaled_run.x_scale = 0.5;
        let scaled = plain_plan(vec![scaled_run]);
        let plain_frame = rasterizer.rasterize(&plain, 0.0).expect("plain frame");
        let scaled_frame = rasterizer.rasterize(&scaled, 0.0).expect("scaled frame");
        assert!(
            scaled_frame.width < plain_frame.width / 2 + 10,
            "tw halves the glyph width and the run advance"
        );
    }

    #[test]
    fn an_anchored_run_moves_the_text_and_grows_the_frame() {
        let rasterizer = test_rasterizer();
        let plain = plain_plan(vec![run("\u{ea01}\u{ea02}", 48.0)]);
        let mut anchored_run = run("\u{ea02}", 48.0);
        anchored_run.anchor = TextAnchor::At { x: 180.0, y: 160.0 };
        let anchored = plain_plan(vec![run("\u{ea01}", 48.0), anchored_run]);
        let plain_frame = rasterizer.rasterize(&plain, 0.0).expect("plain frame");
        let anchored_frame = rasterizer
            .rasterize(&anchored, 0.0)
            .expect("anchored frame");
        assert!(
            anchored_frame.width > plain_frame.width + 100,
            "the second glyph starts 180px to the right"
        );
        assert!(
            anchored_frame.height > plain_frame.height + 100,
            "the second glyph starts 160px lower"
        );
    }

    #[test]
    fn a_run_can_change_the_line_spacing_after_its_line() {
        let rasterizer = test_rasterizer();
        let mut compact = plain_plan(vec![run("\u{ea01}\n\u{ea02}", 48.0)]);
        compact.line_spacing = 0.0;
        let mut spaced = plain_plan(vec![run("\u{ea01}\n\u{ea02}", 48.0)]);
        spaced.runs[0].line_spacing = Some(140.0);
        let compact_frame = rasterizer.rasterize(&compact, 0.0).expect("compact frame");
        let spaced_frame = rasterizer.rasterize(&spaced, 0.0).expect("spaced frame");
        assert!(
            spaced_frame.height > compact_frame.height,
            "<gh> pushes the second line down, which grows the frame"
        );
    }

    #[test]
    fn a_strikethrough_run_draws_a_rule_through_the_row() {
        let rasterizer = test_rasterizer();
        let plain = plain_plan(vec![run("\u{ea01}\u{ea02}", 64.0)]);
        let mut struck_run = run("\u{ea01}\u{ea02}", 64.0);
        struck_run.strikethrough = true;
        let struck = plain_plan(vec![struck_run]);
        let plain_frame = rasterizer.rasterize(&plain, 0.0).expect("plain frame");
        let struck_frame = rasterizer.rasterize(&struck, 0.0).expect("struck frame");
        assert!(
            opaque_pixels(&struck_frame) > opaque_pixels(&plain_frame),
            "the rule adds pixels between and behind the glyphs"
        );
    }

    #[test]
    fn a_run_can_ask_for_its_own_family_and_weight() {
        let mut styled = run("\u{ea01}", 64.0);
        styled.font_family = Some("remixicon".to_owned());
        styled.bold = true;
        styled.italic = true;
        let frame = test_rasterizer()
            .rasterize(&plain_plan(vec![styled]), 0.0)
            .expect("a per-run family resolves through the same fallbacks");
        assert!(opaque_pixels(&frame) > 0);
    }

    fn test_rasterizer() -> TextRasterizer {
        let mut database = Database::new();
        database.load_font_data(include_bytes!("../../../assets/remixicon.ttf").to_vec());
        TextRasterizer {
            database,
            face_bytes: RefCell::new(HashMap::new()),
        }
    }

    #[test]
    fn rasterizes_multiline_text_with_outline_shadow_and_background() {
        let frame = test_rasterizer()
            .rasterize(&plan(), 1.25)
            .expect("bundled outline font rasterizes");
        assert!(frame.width > 64);
        assert!(frame.height > 128);
        assert_eq!(frame.timestamp_seconds, 1.25);
        assert_eq!(
            frame.rgba.len(),
            frame.width as usize * frame.height as usize * 4
        );
        assert!(
            frame
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[3] == 255)
        );
        assert!(
            frame
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[3] == 160)
        );
    }

    #[test]
    fn styled_runs_rasterize_with_their_own_colours_and_sizes() {
        use freya_nle_core::api::TextRun;

        let mut plan = plan();
        plan.content = "<#ff0000>\u{ea01}<#00ff00>\u{ea02}".to_owned();
        plan.outline_enabled = false;
        plan.shadow_enabled = false;
        plan.background_enabled = false;
        plan.line_spacing = 0.0;
        plan.alignment = TextAlignment::Left;
        let red = RgbaColor {
            red: 255,
            green: 0,
            blue: 0,
            alpha: 255,
        };
        let green = RgbaColor {
            red: 0,
            green: 255,
            blue: 0,
            alpha: 255,
        };
        plan.runs = vec![
            TextRun {
                text: "\u{ea01}".to_owned(),
                color: red,
                outline_color: red,
                font_size: 64.0,
                letter_spacing: 0.0,
                line_spacing: None,
                anchor: TextAnchor::Flow,
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
            },
            TextRun {
                text: "\u{ea02}".to_owned(),
                color: green,
                outline_color: green,
                font_size: 128.0,
                letter_spacing: 0.0,
                line_spacing: None,
                anchor: TextAnchor::Flow,
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
            },
        ];
        let frame = test_rasterizer()
            .rasterize(&plan, 0.0)
            .expect("styled runs rasterize");
        let pixels = frame.rgba.as_chunks::<4>().0;
        assert!(
            pixels
                .iter()
                .any(|pixel| pixel[0] == 255 && pixel[1] == 0 && pixel[2] == 0),
            "the first run keeps its red fill"
        );
        assert!(
            pixels
                .iter()
                .any(|pixel| pixel[0] == 0 && pixel[1] == 255 && pixel[2] == 0),
            "the second run keeps its green fill"
        );

        // The taller run raises the line, so the frame grows with it.
        let mut short = plan.clone();
        short.runs[1].font_size = 64.0;
        let short_frame = test_rasterizer()
            .rasterize(&short, 0.0)
            .expect("uniform runs rasterize");
        assert!(frame.height > short_frame.height);
    }

    #[test]
    fn a_run_can_ask_for_a_wider_outline_than_the_object() {
        let mut plan = plan();
        plan.content = "\u{ea01}\u{ea02}".to_owned();
        plan.outline_enabled = true;
        plan.outline_width = 1.0;
        plan.shadow_enabled = false;
        plan.background_enabled = false;
        plan.alignment = TextAlignment::Left;
        plan.runs_authoritative = true;
        let mut thin = run("\u{ea01}", 64.0);
        thin.color = RgbaColor {
            red: 255,
            green: 255,
            blue: 255,
            alpha: 255,
        };
        thin.outline_color = RgbaColor {
            red: 255,
            green: 0,
            blue: 0,
            alpha: 255,
        };
        thin.outline_width = Some(1.0);
        let mut wide = run("\u{ea02}", 64.0);
        wide.color = thin.color;
        // The second run asks for a much wider outline in another colour, so
        // the frame must grow for it and show that colour.
        wide.outline_color = RgbaColor {
            red: 0,
            green: 0,
            blue: 255,
            alpha: 255,
        };
        wide.outline_width = Some(10.0);
        plan.runs = vec![thin.clone(), wide];
        let frame = test_rasterizer()
            .rasterize(&plan, 0.0)
            .expect("the run outline rasterizes");

        // Same runs with the object's thin outline produce a smaller frame.
        plan.runs = vec![thin.clone(), thin];
        let narrow = test_rasterizer()
            .rasterize(&plan, 0.0)
            .expect("the object outline rasterizes");
        assert!(
            frame.width > narrow.width,
            "the wider run outline grows the frame ({} vs {})",
            frame.width,
            narrow.width
        );
        assert!(
            frame
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[2] > 128 && pixel[3] > 0),
            "the wide run's blue outline is drawn"
        );
        // An absurd width is rejected instead of allocating an unbounded raster.
        let mut huge = plan.clone();
        huge.runs[0].outline_width = Some(f32::INFINITY);
        assert_eq!(
            test_rasterizer().rasterize(&huge, 0.0),
            Err(TextRasterError::InvalidParameters)
        );
    }

    #[test]
    fn rejects_unbounded_text_allocations() {
        let mut huge = plan();
        huge.font_size = 9_000.0;
        assert_eq!(
            test_rasterizer().rasterize(&huge, 0.0),
            Err(TextRasterError::InvalidDimensions)
        );
    }

    #[test]
    fn justification_spreads_a_line_across_the_block_width() {
        assert_eq!(
            justified_word_offsets(&[10.0, 10.0], 5.0, 40.0),
            Some(vec![0.0, 30.0])
        );
        // Three words share two gaps.
        assert_eq!(
            justified_word_offsets(&[10.0, 10.0, 10.0], 4.0, 48.0),
            Some(vec![0.0, 19.0, 38.0])
        );
        // A single word, a narrower or equal target, and a non-finite width stay left-aligned.
        assert_eq!(justified_word_offsets(&[10.0], 5.0, 40.0), None);
        assert_eq!(justified_word_offsets(&[10.0, 10.0], 5.0, 25.0), None);
        assert_eq!(justified_word_offsets(&[10.0, 10.0], 5.0, 20.0), None);
        assert_eq!(
            justified_word_offsets(&[10.0, 10.0], 5.0, f32::INFINITY),
            None
        );
    }

    fn shaped_segment(
        glyph_xs: &[f32],
        width: f32,
        spaces: &[usize],
        color: RgbaColor,
    ) -> ShapedSegment {
        ShapedSegment {
            run_index: 0,
            glyphs: glyph_xs
                .iter()
                .map(|x| GlyphPlacement {
                    glyph_id: 1,
                    x: *x,
                    y_offset: 0.0,
                })
                .collect(),
            spaces: spaces.to_vec(),
            width,
            ascent: 64.0,
            scale: 1.0,
            x_scale: 1.0,
            y_scale: 1.0,
            rotation: 0.0,
            color,
            outline_color: color,
            outline_width: None,
            face_index: 0,
            font_size: 64.0,
            strikethrough: false,
            line_spacing: None,
            emoji: None,
        }
    }

    fn shaped_run_line(segments: Vec<ShapedSegment>) -> ShapedRunLine {
        ShapedRunLine {
            width: segments.iter().map(|segment| segment.width).sum(),
            segments,
            anchor: None,
        }
    }

    #[test]
    fn justification_spreads_authoritative_runs_across_their_word_gaps() {
        let ink = RgbaColor {
            red: 0,
            green: 0,
            blue: 0,
            alpha: 255,
        };
        // A plain authoritative run: two gaps, both inside the one segment.
        let mut plain = shaped_run_line(vec![shaped_segment(
            &[0.0, 10.0, 20.0, 30.0, 40.0],
            50.0,
            &[1, 3],
            ink,
        )]);
        assert!(justify_run_line(&mut plain, 90.0));
        assert_eq!(plain.width, 90.0);
        assert_eq!(
            plain.segments[0]
                .glyphs
                .iter()
                .map(|glyph| glyph.x)
                .collect::<Vec<_>>(),
            [0.0, 10.0, 40.0, 50.0, 80.0],
            "each gap takes half of the twenty spare pixels"
        );
        assert_eq!(plain.segments[0].width, 90.0);
    }

    #[test]
    fn justification_follows_a_gap_across_a_styled_run_boundary() {
        let red = RgbaColor {
            red: 255,
            green: 0,
            blue: 0,
            alpha: 255,
        };
        let green = RgbaColor {
            red: 0,
            green: 255,
            blue: 0,
            alpha: 255,
        };
        // "ab " (red) + "cd" (green): the only gap ends the red run.
        let mut boundary = shaped_run_line(vec![
            shaped_segment(&[0.0, 10.0, 20.0], 30.0, &[2], red),
            shaped_segment(&[0.0, 10.0], 20.0, &[], green),
        ]);
        assert!(justify_run_line(&mut boundary, 60.0));
        assert_eq!(boundary.width, 60.0);
        assert_eq!(boundary.segments[0].width, 40.0);
        assert_eq!(
            boundary.segments[1]
                .glyphs
                .iter()
                .map(|glyph| glyph.x)
                .collect::<Vec<_>>(),
            [0.0, 10.0],
            "the following run retains segment-local positions"
        );

        // "ab" (red) + "c d" (green): the gap is inside the second run.
        let mut inside = shaped_run_line(vec![
            shaped_segment(&[0.0, 10.0], 20.0, &[], red),
            shaped_segment(&[0.0, 10.0, 20.0], 30.0, &[1], green),
        ]);
        assert!(justify_run_line(&mut inside, 70.0));
        assert_eq!(inside.segments[0].width, 20.0);
        assert_eq!(
            inside.segments[1]
                .glyphs
                .iter()
                .map(|glyph| glyph.x)
                .collect::<Vec<_>>(),
            [0.0, 10.0, 40.0],
            "the space stays where it is and the next word moves over"
        );
        assert_eq!(inside.segments[1].width, 50.0);
    }

    #[test]
    fn a_line_without_eligible_word_gaps_keeps_its_natural_width() {
        let ink = RgbaColor {
            red: 0,
            green: 0,
            blue: 0,
            alpha: 255,
        };
        let mut no_gaps = shaped_run_line(vec![
            shaped_segment(&[0.0, 10.0], 20.0, &[], ink),
            shaped_segment(&[0.0, 10.0], 20.0, &[], ink),
        ]);
        assert!(!justify_run_line(&mut no_gaps, 100.0));
        assert_eq!(no_gaps.width, 40.0);
        assert_eq!(no_gaps.segments[1].width, 20.0);

        // A single word is not stretched even when it is narrower.
        let mut one_word = shaped_run_line(vec![shaped_segment(&[0.0, 10.0], 20.0, &[], ink)]);
        assert!(!justify_run_line(&mut one_word, 100.0));
        assert_eq!(one_word.width, 20.0);
    }

    #[test]
    fn a_trailing_space_is_not_a_word_gap() {
        let ink = RgbaColor {
            red: 0,
            green: 0,
            blue: 0,
            alpha: 255,
        };
        // "A " ends in a space with no text after it, so the line has no word
        // gap and must not stretch (which would drag its strikethrough out to
        // the block width).
        let mut trailing = shaped_run_line(vec![shaped_segment(&[0.0, 10.0], 20.0, &[1], ink)]);
        assert!(!justify_run_line(&mut trailing, 60.0));
        assert_eq!(trailing.width, 20.0);
        assert_eq!(
            trailing.segments[0]
                .glyphs
                .iter()
                .map(|glyph| glyph.x)
                .collect::<Vec<_>>(),
            [0.0, 10.0]
        );

        // "A B " has one real gap and one trailing space: only the real gap is
        // spread, and the trailing space rides along with the word before it.
        let mut mixed = shaped_run_line(vec![shaped_segment(
            &[0.0, 10.0, 20.0, 30.0],
            40.0,
            &[1, 3],
            ink,
        )]);
        assert!(justify_run_line(&mut mixed, 60.0));
        assert_eq!(mixed.width, 60.0);
        assert_eq!(
            mixed.segments[0]
                .glyphs
                .iter()
                .map(|glyph| glyph.x)
                .collect::<Vec<_>>(),
            [0.0, 10.0, 40.0, 50.0],
            "the one real gap takes the whole twenty spare pixels"
        );
    }

    #[test]
    fn an_emoji_after_a_space_is_content() {
        let ink = RgbaColor {
            red: 0,
            green: 0,
            blue: 0,
            alpha: 255,
        };
        // "ab " + emoji: the emoji follows the space, so the space is a real
        // word gap even though the emoji segment carries no glyphs of its own.
        let mut emoji = shaped_segment(&[], 10.0, &[], ink);
        emoji.emoji = Some("__test_star".to_owned());
        let mut line = shaped_run_line(vec![
            shaped_segment(&[0.0, 10.0, 20.0], 30.0, &[2], ink),
            emoji,
        ]);
        assert!(justify_run_line(&mut line, 60.0));
        assert_eq!(line.width, 60.0);
        assert_eq!(
            line.segments[0].width, 50.0,
            "the gap before the emoji takes the whole twenty spare pixels"
        );
        assert_eq!(line.segments[1].width, 10.0, "the emoji cell is unchanged");
    }

    #[test]
    fn the_last_authoritative_line_keeps_its_natural_width() {
        let ink = RgbaColor {
            red: 0,
            green: 0,
            blue: 0,
            alpha: 255,
        };
        let mut lines = vec![
            shaped_run_line(vec![shaped_segment(&[0.0, 10.0, 20.0], 30.0, &[1], ink)]),
            shaped_run_line(vec![shaped_segment(&[0.0, 10.0, 20.0], 30.0, &[1], ink)]),
        ];
        justify_run_lines(&mut lines, 60.0);
        assert_eq!(lines[0].width, 60.0, "the first line is spread");
        assert_eq!(
            lines[1].width, 30.0,
            "the last line of the block keeps its natural width"
        );
    }

    #[test]
    fn alignment_offsets_follow_qt_horizontal_flags() {
        assert_eq!(alignment_offset(TextAlignment::Left, 100.0, 40.0), 0.0);
        assert_eq!(alignment_offset(TextAlignment::Justify, 100.0, 40.0), 0.0);
        assert_eq!(alignment_offset(TextAlignment::Center, 100.0, 40.0), 30.0);
        assert_eq!(alignment_offset(TextAlignment::Right, 100.0, 40.0), 60.0);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn system_catalog_rasterizes_the_builtin_japanese_default() {
        let mut japanese = plan();
        japanese.content = "テキスト".to_owned();
        japanese.font_family = "sans-serif".to_owned();
        let frame = TextRasterizer::new()
            .rasterize(&japanese, 0.0)
            .expect("macOS system fonts cover the built-in Japanese text");
        assert!(
            frame
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[3] > 0)
        );
    }
}
