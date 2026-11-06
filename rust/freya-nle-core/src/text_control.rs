//! AviUtl text control characters.
//!
//! The text object accepts inline directives such as `<#ff8800>`, `<s32>`,
//! `<r10>` or `<c2>`. This module parses one string into styled runs and, when
//! the timing directives are present, into the runs that are visible at a
//! given time.
//!
//! Supported:
//!
//! - `<#RRGGBB>` / `<#RRGGBB,RRGGBB>`: text colour and optionally the outline
//!   colour, with `<#>` restoring the object's colours;
//! - `<sNN>`, `<s+NN>`, `<s-NN>`, `<s*1.5>`: absolute or relative font size,
//!   with `<s>` restoring the object size;
//! - `<gwNN>`: letter spacing, with `<gw>` restoring the object spacing;
//! - `<ghNN>`: line spacing, with `<gh>` restoring the object's line spacing;
//! - `<pX,Y>`: starts a positioned line at (`X`,`Y`) instead of following the
//!   previous character; `<p>` returns to the normal flow. A number with a
//!   sign is relative to the previous position;
//! - `<twN>` / `<thN>`: horizontal and vertical glyph scale, with `<tw>` /
//!   `<th>` restoring 1.0;
//! - `<trN>`: rotation in degrees, with a signed value relative to the current
//!   angle and `<tr>` restoring 0;
//! - `</>base<![scale][+]>ruby</>`: furigana over the base text, where `+`
//!   expands the line so the ruby does not collide with the line above.
//! - `<@Family[,3BI]>`: font family with an optional decoration digit and
//!   `B`/`I`/`S` style letters; `<@>` restores the object's font, while
//!   `<@+B>` / `<@-B>` add or remove one style on the current font;
//! - `<sNN,Family,BI>`: the extended size form, which also sets the family and
//!   style letters (AviUtl's trailing outline size is accepted and ignored);
//! - `<// comment //>`: a comment that renders nothing, and `</>`: a block
//!   separator that renders nothing until ruby needs the boundaries;
//! - `<rNN>`: characters per second, with `<r>` restoring the object speed and
//!   `<r0>` revealing instantly;
//! - `<wNN>` / `<w*0.2>`: wait before the following characters, the `*` form
//!   multiplying the seconds by the characters shown so far;
//! - `<cNN>` / `<c*0.2>`: clear the current block after the wait, then show the
//!   following block; `<c>` clears immediately.
//!
//! Anything else between `<` and `>` stays literal text.

use crate::bake_plan::RgbaColor;
use crate::keyframe_document::parse_hex_color;

/// One run of text after control characters have been applied.
#[derive(Debug, Clone, PartialEq)]
pub struct TextRun {
    pub text: String,
    pub color: RgbaColor,
    pub outline_color: RgbaColor,
    pub font_size: f32,
    pub letter_spacing: f32,
    /// Family requested by `<@...>`; `None` keeps the object's family.
    pub font_family: Option<String>,
    pub bold: bool,
    pub italic: bool,
    pub strikethrough: bool,
    /// Line spacing for the gap after this run's line, when `<gh...>` set one.
    pub line_spacing: Option<f32>,
    /// Where `<p...>` put the text that starts with this run.
    pub anchor: TextAnchor,
    /// Horizontal and vertical glyph scale from `<tw...>` / `<th...>`.
    pub x_scale: f32,
    pub y_scale: f32,
    /// Rotation in degrees from `<tr...>`.
    pub rotation: f32,
    /// Ruby (furigana) block this run belongs to, when it is inside `</>...`.
    pub ruby: Option<RubyRun>,
}

/// One run's part in a `</>base<![...]>ruby</>` block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RubyRun {
    /// Pairs the base text with its annotation.
    pub block: u32,
    /// `true` for the base text, `false` for the furigana itself.
    pub base: bool,
    /// Furigana size relative to the base font size.
    pub scale: f32,
    /// Whether the line grows to fit the furigana.
    pub expand: bool,
}

/// The text's position after a `<p...>` directive.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum TextAnchor {
    /// Keep flowing from the previous character.
    #[default]
    Flow,
    /// Start a line at this point, measured in pixels from the text origin.
    At { x: f32, y: f32 },
    /// `<p>`: return to the object's normal flow.
    Reset,
}

/// The values a run starts from when no directive has changed them.
#[derive(Debug, Clone, PartialEq)]
pub struct TextBaseStyle {
    pub color: RgbaColor,
    pub outline_color: RgbaColor,
    pub font_size: f32,
    pub letter_spacing: f32,
    /// The object's own family, restored by `<@>`.
    pub font_family: Option<String>,
    pub bold: bool,
    pub italic: bool,
    /// Line spacing requested by `<gh...>`; `None` keeps the object's value.
    pub line_spacing: Option<f32>,
    /// The object's own anchor, restored by `<p>`.
    pub anchor: TextAnchor,
    pub x_scale: f32,
    pub y_scale: f32,
    pub rotation: f32,
    pub ruby: Option<RubyRun>,
}

/// The font half of a run: family and the three style switches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FontStyle {
    family: Option<String>,
    bold: bool,
    italic: bool,
    strikethrough: bool,
}

impl FontStyle {
    fn from_base(base: &TextBaseStyle) -> Self {
        Self {
            family: base.font_family.clone(),
            bold: base.bold,
            italic: base.italic,
            strikethrough: false,
        }
    }
}

const MIN_FONT_SIZE: f32 = 1.0;
const MAX_FONT_SIZE: f32 = 4_096.0;
const MAX_LETTER_SPACING: f32 = 1_000.0;
const MAX_LINE_SPACING: f32 = 4_096.0;
const MAX_POSITION: f32 = 32_768.0;
const MAX_GLYPH_SCALE: f32 = 100.0;
const MAX_ROTATION: f32 = 3_600.0;
const RUBY_DEFAULT_SCALE: f32 = 0.5;
const RUBY_MIN_SCALE: f32 = 0.05;
const RUBY_MAX_SCALE: f32 = 4.0;
const MAX_SPEED: f32 = 1_000.0;
const MAX_WAIT_SECONDS: f32 = 3_600.0;

/// Splits one text string into styled runs at an unlimited time.
///
/// This is the untimed view used by callers that only need the styling; the
/// frame planner uses [`parse_text_runs_at`].
#[cfg(test)]
pub fn parse_text_runs(input: &str, base: TextBaseStyle) -> Vec<TextRun> {
    parse_text_runs_at(input, base, 0.0, f32::INFINITY)
}

/// The runs visible at `time` seconds after the object starts.
///
/// `base_speed` is the object's own display speed; zero reveals the text
/// immediately and is also what a bare `<r>` restores.
pub fn parse_text_runs_at(
    input: &str,
    base: TextBaseStyle,
    base_speed: f32,
    time: f32,
) -> Vec<TextRun> {
    let base_speed = if base_speed.is_finite() {
        base_speed.clamp(0.0, MAX_SPEED)
    } else {
        0.0
    };
    let blocks = parse_blocks(input, base, base_speed);
    visible_runs(&blocks, time)
}

#[derive(Debug, Clone)]
struct TimedChar {
    character: char,
    show_at: f32,
}

#[derive(Debug, Clone)]
struct TimedRun {
    style: TextBaseStyle,
    font: FontStyle,
    characters: Vec<TimedChar>,
}

#[derive(Debug, Clone)]
struct TextBlock {
    start: f32,
    clear_at: Option<f32>,
    runs: Vec<TimedRun>,
}

struct Parser {
    base: TextBaseStyle,
    style: TextBaseStyle,
    font: FontStyle,
    base_speed: f32,
    speed: f32,
    cursor: f32,
    block_start: f32,
    displayed: usize,
    runs: Vec<TimedRun>,
    blocks: Vec<TextBlock>,
    skip_newline: bool,
    /// Current `</>...` state and the id handed to the next block.
    ruby: RubyState,
    next_ruby_block: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum RubyState {
    None,
    Base {
        block: u32,
    },
    Ruby {
        block: u32,
        scale: f32,
        expand: bool,
    },
}

impl Parser {
    fn new(base: TextBaseStyle, base_speed: f32) -> Self {
        let font = FontStyle::from_base(&base);
        Self {
            style: base.clone(),
            base,
            font,
            base_speed,
            speed: base_speed,
            cursor: 0.0,
            block_start: 0.0,
            displayed: 0,
            runs: Vec::new(),
            blocks: Vec::new(),
            skip_newline: false,
            ruby: RubyState::None,
            next_ruby_block: 0,
        }
    }

    fn push_char(&mut self, character: char) {
        let needs_run = self
            .runs
            .last()
            .is_none_or(|run| run.style != self.style || run.font != self.font);
        if needs_run {
            self.runs.push(TimedRun {
                style: self.style.clone(),
                font: self.font.clone(),
                characters: Vec::new(),
            });
        }
        if let Some(run) = self.runs.last_mut() {
            run.characters.push(TimedChar {
                character,
                show_at: self.cursor,
            });
        }
        if character != '\n' {
            self.displayed = self.displayed.saturating_add(1);
        }
        if self.speed > 0.0 {
            self.cursor += 1.0 / self.speed;
        }
    }

    fn apply_directive(&mut self, directive: &str) -> bool {
        let Some(marker) = directive.chars().next() else {
            return false;
        };
        match marker {
            '#' => self.apply_color(&directive[1..]),
            's' => self.apply_size(&directive[1..]),
            '@' => self.apply_font(&directive[1..]),
            'p' => self.apply_position(&directive[1..]),
            't' => self.apply_transform(&directive[1..]),
            'g' => self.apply_spacing(&directive[1..]),
            'r' => self.apply_speed(&directive[1..]),
            'w' => self.apply_wait(&directive[1..]),
            'c' => self.apply_clear(&directive[1..]),
            // `</>` opens or closes a ruby block.
            '/' => self.apply_ruby_boundary(directive.len() == 1),
            '!' => self.apply_ruby_options(&directive[1..]),
            _ => false,
        }
    }

    fn apply_color(&mut self, value: &str) -> bool {
        let mut parts = value.split(',');
        let text = parts.next().unwrap_or_default();
        let outline = parts.next();
        if parts.next().is_some() {
            return false;
        }
        let color = if text.is_empty() {
            self.base.color
        } else {
            match parse_color(text) {
                Some(color) => color,
                None => return false,
            }
        };
        let outline_color = match outline {
            None => self.style.outline_color,
            Some(value) if value.trim().is_empty() => self.base.outline_color,
            Some(value) => match parse_color(value) {
                Some(color) => color,
                None => return false,
            },
        };
        self.style.color = color;
        self.style.outline_color = outline_color;
        true
    }

    fn apply_size(&mut self, value: &str) -> bool {
        let mut parts = value.split(',');
        let size = parts.next().unwrap_or_default().trim();
        let font_size = if size.is_empty() {
            self.base.font_size
        } else {
            match resolve_size(size, self.style.font_size) {
                Some(size) => size,
                None => return false,
            }
        };
        let family = match parts.next().map(str::trim) {
            None | Some("") => None,
            Some(family) => match normalize_family(family) {
                Some(family) => Some(family),
                None => return false,
            },
        };
        let decoration = match parts.next() {
            None => None,
            Some(decoration) => match parse_decoration(decoration) {
                Some(styles) => Some(styles),
                None => return false,
            },
        };
        // AviUtl's fourth field is the outline size, which this build keeps
        // per effect rather than per run; it is accepted and ignored.
        let _outline_size = parts.next();
        if parts.next().is_some() {
            return false;
        }
        self.style.font_size = font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        if let Some(family) = family {
            self.font.family = Some(family);
        }
        if let Some((bold, italic, strikethrough)) = decoration {
            self.font.bold = bold;
            self.font.italic = italic;
            self.font.strikethrough = strikethrough;
        }
        true
    }

    /// `</>`: opens a ruby block on first use and closes it on second.
    fn apply_ruby_boundary(&mut self, bare: bool) -> bool {
        if !bare {
            return false;
        }
        match self.ruby {
            RubyState::None => {
                self.next_ruby_block = self.next_ruby_block.wrapping_add(1);
                let block = self.next_ruby_block;
                self.ruby = RubyState::Base { block };
                self.style.ruby = Some(RubyRun {
                    block,
                    base: true,
                    scale: RUBY_DEFAULT_SCALE,
                    expand: false,
                });
            }
            RubyState::Ruby { .. } => {
                self.ruby = RubyState::None;
                self.style.ruby = None;
            }
            RubyState::Base { .. } => return false,
        }
        true
    }

    /// `<!>` / `<!0.4>` / `<!+>`: starts the furigana text of the open block.
    fn apply_ruby_options(&mut self, value: &str) -> bool {
        let RubyState::Base { block } = self.ruby else {
            return false;
        };
        let value = value.trim();
        let (scale, expand) = match value.strip_suffix('+') {
            Some(scale) => (scale.trim(), true),
            None => (value, false),
        };
        let scale = if scale.is_empty() {
            RUBY_DEFAULT_SCALE
        } else {
            match scale.parse::<f32>() {
                Ok(scale) if scale.is_finite() && scale > 0.0 => {
                    scale.clamp(RUBY_MIN_SCALE, RUBY_MAX_SCALE)
                }
                _ => return false,
            }
        };
        self.ruby = RubyState::Ruby {
            block,
            scale,
            expand,
        };
        self.style.ruby = Some(RubyRun {
            block,
            base: false,
            scale,
            expand,
        });
        true
    }

    /// `<twN>` / `<thN>`: horizontal and vertical glyph scale.
    fn apply_transform(&mut self, value: &str) -> bool {
        let Some((axis, value)) = value.split_at_checked(1) else {
            return false;
        };
        let value = value.trim();
        let scale = if value.is_empty() {
            1.0
        } else {
            match value.parse::<f32>() {
                Ok(value) if value.is_finite() => value.clamp(0.0, MAX_GLYPH_SCALE),
                _ => return false,
            }
        };
        if axis == "r" {
            let rotation = if value.is_empty() {
                0.0
            } else {
                let (text, relative) = match value.strip_prefix('+') {
                    Some(value) => (value.trim(), true),
                    None => (value, false),
                };
                let Ok(value) = text.parse::<f32>() else {
                    return false;
                };
                if !value.is_finite() {
                    return false;
                }
                let value = if relative {
                    self.style.rotation + value
                } else {
                    value
                };
                value.clamp(-MAX_ROTATION, MAX_ROTATION)
            };
            self.style.rotation = rotation;
            return true;
        }
        match axis {
            "w" => self.style.x_scale = scale,
            "h" => self.style.y_scale = scale,
            _ => return false,
        }
        true
    }

    /// `<pX,Y[,Z]>`: start a positioned line; `<p>` returns to the flow.
    ///
    /// A coordinate with an explicit sign is relative to the position in
    /// effect, so `<p+10>` nudges the text ten pixels right; an empty field
    /// keeps the previous value, which lets `<p,40>` move only vertically.
    fn apply_position(&mut self, value: &str) -> bool {
        let value = value.trim();
        if value.is_empty() {
            self.style.anchor = TextAnchor::Reset;
            return true;
        }
        let mut fields = value.split(',');
        let x_field = fields.next().unwrap_or_default();
        let y_field = fields.next().unwrap_or_default();
        let z_field = fields.next();
        if fields.next().is_some() {
            return false;
        }
        let (current_x, current_y) = match self.style.anchor {
            TextAnchor::At { x, y } => (x, y),
            _ => (0.0, 0.0),
        };
        let Some(x) = position_coordinate(x_field, current_x) else {
            return false;
        };
        let Some(y) = position_coordinate(y_field, current_y) else {
            return false;
        };
        if let Some(z) = z_field
            && position_coordinate(z, 0.0).is_none()
        {
            return false;
        }
        self.style.anchor = TextAnchor::At {
            x: x.unwrap_or(current_x),
            y: y.unwrap_or(current_y),
        };
        true
    }

    fn apply_font(&mut self, value: &str) -> bool {
        let value = value.trim();
        if value.is_empty() {
            self.font = FontStyle::from_base(&self.base);
            return true;
        }
        if let Some(letters) = value.strip_prefix('+') {
            return self.apply_style_letters(letters, true);
        }
        if let Some(letters) = value.strip_prefix('-') {
            return self.apply_style_letters(letters, false);
        }
        let mut parts = value.split(',');
        let family = parts.next().unwrap_or_default().trim();
        let decoration = parts.next();
        if parts.next().is_some() {
            return false;
        }
        let family = if family.is_empty() {
            self.base.font_family.clone()
        } else {
            match normalize_family(family) {
                Some(family) => Some(family),
                None => return false,
            }
        };
        let mut font = self.font.clone();
        font.family = family;
        if let Some(decoration) = decoration {
            let Some((bold, italic, strikethrough)) = parse_decoration(decoration) else {
                return false;
            };
            font.bold = bold;
            font.italic = italic;
            font.strikethrough = strikethrough;
        }
        self.font = font;
        true
    }

    fn apply_style_letters(&mut self, letters: &str, add: bool) -> bool {
        let letters = letters.trim();
        if letters.is_empty() {
            return false;
        }
        let mut font = self.font.clone();
        for letter in letters.chars() {
            match letter.to_ascii_uppercase() {
                'B' => font.bold = add,
                'I' => font.italic = add,
                'S' => font.strikethrough = add,
                _ => return false,
            }
        }
        self.font = font;
        true
    }

    fn apply_spacing(&mut self, value: &str) -> bool {
        if let Some(spacing) = value.strip_prefix('h') {
            let spacing = spacing.trim();
            self.style.line_spacing = if spacing.is_empty() {
                self.base.line_spacing
            } else {
                match spacing.parse::<f32>() {
                    Ok(value) if value.is_finite() => Some(value.clamp(0.0, MAX_LINE_SPACING)),
                    _ => return false,
                }
            };
            return true;
        }
        let Some(spacing) = value.strip_prefix('w') else {
            return false;
        };
        let spacing = spacing.trim();
        let letter_spacing = if spacing.is_empty() {
            self.base.letter_spacing
        } else {
            match spacing.parse::<f32>() {
                Ok(value) if value.is_finite() => {
                    value.clamp(-MAX_LETTER_SPACING, MAX_LETTER_SPACING)
                }
                _ => return false,
            }
        };
        self.style.letter_spacing = letter_spacing;
        true
    }

    fn apply_speed(&mut self, value: &str) -> bool {
        let value = value.trim();
        let speed = if value.is_empty() {
            self.base_speed
        } else {
            match value.parse::<f32>() {
                Ok(speed) if speed.is_finite() && speed >= 0.0 => speed.clamp(0.0, MAX_SPEED),
                _ => return false,
            }
        };
        self.speed = speed;
        true
    }

    fn apply_wait(&mut self, value: &str) -> bool {
        let value = value.trim();
        if value.is_empty() {
            return true;
        }
        let Some(seconds) = parse_scaled_seconds(value, self.displayed) else {
            return false;
        };
        self.cursor += seconds.clamp(0.0, MAX_WAIT_SECONDS);
        true
    }

    fn apply_clear(&mut self, value: &str) -> bool {
        let value = value.trim();
        let delay = if value.is_empty() {
            0.0
        } else {
            match parse_scaled_seconds(value, self.displayed) {
                Some(delay) => delay.clamp(0.0, MAX_WAIT_SECONDS),
                None => return false,
            }
        };
        let clear_at = self.cursor + delay;
        let runs = std::mem::take(&mut self.runs);
        self.blocks.push(TextBlock {
            start: self.block_start,
            clear_at: Some(clear_at),
            runs,
        });
        self.block_start = clear_at;
        self.cursor = clear_at;
        self.displayed = 0;
        // AviUtl ignores the newline directly after a clear directive.
        self.skip_newline = true;
        true
    }

    fn finish(mut self) -> Vec<TextBlock> {
        if !self.runs.is_empty() || self.blocks.is_empty() {
            self.blocks.push(TextBlock {
                start: self.block_start,
                clear_at: None,
                runs: std::mem::take(&mut self.runs),
            });
        }
        self.blocks
    }
}

fn parse_blocks(input: &str, base: TextBaseStyle, base_speed: f32) -> Vec<TextBlock> {
    let mut parser = Parser::new(base, base_speed);
    let mut index = 0;
    while index < input.len() {
        // Comments may contain `>`, so they are skipped before the directive
        // scan would stop at the first one.
        if input[index..].starts_with("<//")
            && let Some(relative_end) = input[index + 3..].find("//>")
        {
            index += 3 + relative_end + 3;
            continue;
        }
        if parser.skip_newline {
            parser.skip_newline = false;
            if input[index..].starts_with('\n') {
                index += 1;
                continue;
            }
        }
        if input.as_bytes()[index] == b'<'
            && let Some(relative_end) = input[index + 1..].find('>')
        {
            let directive_end = index + 1 + relative_end;
            if parser.apply_directive(&input[index + 1..directive_end]) {
                index = directive_end + 1;
                continue;
            }
        }
        let character = input[index..]
            .chars()
            .next()
            .expect("index points at a character boundary");
        parser.push_char(character);
        index += character.len_utf8();
    }
    parser.finish()
}

fn visible_runs(blocks: &[TextBlock], time: f32) -> Vec<TextRun> {
    let Some(block) = blocks.iter().rev().find(|block| block.start <= time) else {
        return Vec::new();
    };
    if block.clear_at.is_some_and(|clear| time >= clear) {
        return Vec::new();
    }
    let mut runs = Vec::new();
    for run in &block.runs {
        let text = run
            .characters
            .iter()
            .take_while(|character| character.show_at <= time)
            .map(|character| character.character)
            .collect::<String>();
        if text.is_empty() {
            continue;
        }
        runs.push(TextRun {
            text,
            color: run.style.color,
            outline_color: run.style.outline_color,
            font_size: run.style.font_size,
            letter_spacing: run.style.letter_spacing,
            line_spacing: run.style.line_spacing,
            anchor: run.style.anchor,
            x_scale: run.style.x_scale,
            y_scale: run.style.y_scale,
            rotation: run.style.rotation,
            ruby: run.style.ruby,
            font_family: run.font.family.clone(),
            bold: run.font.bold,
            italic: run.font.italic,
            strikethrough: run.font.strikethrough,
        });
    }
    runs
}

/// Validates a family name from a control character.
///
/// `<` and `>` never appear inside a directive, and a control character would
/// make the font query meaningless, so both reject the directive rather than
/// passing it on.
fn normalize_family(family: &str) -> Option<String> {
    let family = family.trim();
    if family.is_empty()
        || family.chars().count() > 128
        || family
            .chars()
            .any(|character| character == '<' || character == '>' || character.is_control())
    {
        return None;
    }
    Some(family.to_owned())
}

/// Parses `3BI`-style decoration specs into the three style switches.
///
/// The leading digit is AviUtl's shadow/outline decoration (0 = standard,
/// 1-6 = outline variants). Only the digit's presence matters here: per-run
/// outline styles would need every run to carry the object's outline
/// settings, so the object's outline still applies and the digit is accepted
/// without changing it.
fn parse_decoration(spec: &str) -> Option<(bool, bool, bool)> {
    let spec = spec.trim();
    let mut rest = spec;
    let mut digits = 0;
    while rest.starts_with(|character: char| character.is_ascii_digit()) {
        rest = &rest[1..];
        digits += 1;
    }
    if digits > 1 {
        return None;
    }
    let mut styles = (false, false, false);
    for letter in rest.chars() {
        match letter.to_ascii_uppercase() {
            'B' => styles.0 = true,
            'I' => styles.1 = true,
            'S' => styles.2 = true,
            _ => return None,
        }
    }
    Some(styles)
}

/// Parses one `<p>` coordinate; `None` means "keep the previous value", the
/// outer `Option` is the parse result.
fn position_coordinate(field: &str, current: f32) -> Option<Option<f32>> {
    let field = field.trim();
    if field.is_empty() {
        return Some(None);
    }
    let (text, relative) = match field.strip_prefix('+') {
        Some(value) => (value.trim(), true),
        None => (field, false),
    };
    let value = text.parse::<f32>().ok().filter(|value| value.is_finite())?;
    let value = if relative { current + value } else { value };
    Some(Some(value.clamp(-MAX_POSITION, MAX_POSITION)))
}

fn parse_scaled_seconds(value: &str, displayed: usize) -> Option<f32> {
    let (value, scale) = match value.strip_prefix('*') {
        Some(value) => (value.trim(), true),
        None => (value, false),
    };
    let value = value
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())?;
    if value < 0.0 {
        return None;
    }
    Some(if scale {
        value * displayed as f32
    } else {
        value
    })
}

fn resolve_size(value: &str, current: f32) -> Option<f32> {
    let size = if let Some(factor) = value.strip_prefix('*') {
        current * factor.trim().parse::<f32>().ok()?
    } else if let Some(delta) = value.strip_prefix('+') {
        current + delta.trim().parse::<f32>().ok()?
    } else if let Some(delta) = value.strip_prefix('-') {
        current - delta.trim().parse::<f32>().ok()?
    } else {
        value.parse::<f32>().ok()?
    };
    size.is_finite().then_some(size)
}

fn parse_color(value: &str) -> Option<RgbaColor> {
    let value = value.trim();
    if let Some([alpha, red, green, blue]) = named_color(value) {
        return Some(RgbaColor {
            red,
            green,
            blue,
            alpha,
        });
    }
    let hex = if value.starts_with('#') {
        value.to_owned()
    } else {
        format!("#{value}")
    };
    let [alpha, red, green, blue] = parse_hex_color(&hex)?;
    Some(RgbaColor {
        red,
        green,
        blue,
        alpha,
    })
}

/// A fixed handful of common colour names.
///
/// AviUtl's named colours are user presets, which this build has no palette for
/// yet; these are the everyday names, and an unknown name keeps the directive
/// as literal text instead of guessing a colour.
fn named_color(value: &str) -> Option<[u8; 4]> {
    let [red, green, blue, alpha] = match value.to_ascii_lowercase().as_str() {
        "white" => [0xff, 0xff, 0xff, 0xff],
        "black" => [0x00, 0x00, 0x00, 0xff],
        "red" => [0xff, 0x00, 0x00, 0xff],
        "green" => [0x00, 0xff, 0x00, 0xff],
        "blue" => [0x00, 0x00, 0xff, 0xff],
        "yellow" => [0xff, 0xff, 0x00, 0xff],
        "cyan" => [0x00, 0xff, 0xff, 0xff],
        "magenta" => [0xff, 0x00, 0xff, 0xff],
        "orange" => [0xff, 0xa5, 0x00, 0xff],
        "gray" | "grey" => [0x80, 0x80, 0x80, 0xff],
        _ => return None,
    };
    Some([alpha, red, green, blue])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> TextBaseStyle {
        TextBaseStyle {
            color: RgbaColor {
                red: 0xff,
                green: 0xff,
                blue: 0xff,
                alpha: 0xff,
            },
            outline_color: RgbaColor {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 0xff,
            },
            font_size: 48.0,
            letter_spacing: 0.0,
            line_spacing: None,
            anchor: TextAnchor::Flow,
            x_scale: 1.0,
            y_scale: 1.0,
            rotation: 0.0,
            ruby: None,
            font_family: None,
            bold: false,
            italic: false,
        }
    }

    fn color(run: &TextRun) -> (u8, u8, u8) {
        (run.color.red, run.color.green, run.color.blue)
    }

    fn text(runs: &[TextRun]) -> String {
        runs.iter().map(|run| run.text.as_str()).collect()
    }

    #[test]
    fn plain_text_is_one_run() {
        let runs = parse_text_runs("テキスト", base());
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "テキスト");
        assert_eq!(runs[0].font_size, 48.0);
    }

    #[test]
    fn colour_directives_split_runs_and_reset_to_the_base() {
        let runs = parse_text_runs("<#ff0000>red<#>plain", base());
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text, "red");
        assert_eq!(color(&runs[0]), (0xff, 0x00, 0x00));
        assert_eq!(runs[1].text, "plain");
        assert_eq!(color(&runs[1]), (0xff, 0xff, 0xff));
    }

    #[test]
    fn two_colours_set_the_outline_too() {
        let runs = parse_text_runs("<#112233,445566>both", base());
        assert_eq!(runs.len(), 1);
        assert_eq!(color(&runs[0]), (0x11, 0x22, 0x33));
        assert_eq!(
            (
                runs[0].outline_color.red,
                runs[0].outline_color.green,
                runs[0].outline_color.blue
            ),
            (0x44, 0x55, 0x66)
        );
    }

    #[test]
    fn named_colours_work_and_unknown_names_stay_literal() {
        let runs = parse_text_runs("<#red>red <#chartreuse>?", base());
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "red <#chartreuse>?");
        assert_eq!(color(&runs[0]), (0xff, 0x00, 0x00));
    }

    #[test]
    fn size_directives_are_absolute_or_relative_to_the_current_size() {
        let runs = parse_text_runs("<s32>a<s+8>b<s*2>c<s>d", base());
        assert_eq!(runs.len(), 4);
        assert_eq!(runs[0].font_size, 32.0);
        assert_eq!(runs[1].font_size, 40.0);
        assert_eq!(runs[2].font_size, 80.0);
        assert_eq!(runs[3].font_size, 48.0);
    }

    #[test]
    fn size_values_are_clamped_and_bad_values_stay_literal() {
        let runs = parse_text_runs("<s0>a<s99999>b", base());
        assert_eq!(runs[0].font_size, 1.0);
        assert_eq!(runs[1].font_size, 4_096.0);
        let literal = parse_text_runs("<sabc>text", base());
        assert_eq!(literal.len(), 1);
        assert_eq!(literal[0].text, "<sabc>text");
    }

    #[test]
    fn letter_spacing_directives_change_runs() {
        let runs = parse_text_runs("<gw4>spaced<gw>normal", base());
        assert_eq!(runs[0].letter_spacing, 4.0);
        assert_eq!(runs[1].letter_spacing, 0.0);
    }

    #[test]
    fn directives_across_newlines_keep_the_text() {
        let runs = parse_text_runs("a\n<s64>b\nc", base());
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text, "a\n");
        assert_eq!(runs[1].text, "b\nc");
        assert_eq!(runs[1].font_size, 64.0);
    }

    #[test]
    fn an_unclosed_directive_is_literal() {
        let runs = parse_text_runs("<#ff0000", base());
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "<#ff0000");
    }

    #[test]
    fn speed_reveals_one_character_at_a_time() {
        let input = "<r2>abcd";
        assert_eq!(text(&parse_text_runs_at(input, base(), 0.0, 0.0)), "a");
        assert_eq!(text(&parse_text_runs_at(input, base(), 0.0, 0.49)), "a");
        assert_eq!(text(&parse_text_runs_at(input, base(), 0.0, 0.5)), "ab");
        assert_eq!(text(&parse_text_runs_at(input, base(), 0.0, 1.0)), "abc");
        assert_eq!(text(&parse_text_runs_at(input, base(), 0.0, 1.5)), "abcd");
        // `<r0>` reveals instantly from that point on, which is the display
        // cursor's time (1 s here), not the object's start.
        let instant = "<r2>ab<r0>cd";
        assert_eq!(text(&parse_text_runs_at(instant, base(), 0.0, 0.5)), "ab");
        assert_eq!(text(&parse_text_runs_at(instant, base(), 0.0, 1.0)), "abcd");
    }

    #[test]
    fn a_wait_delays_the_following_characters() {
        let input = "ab<w1>cd";
        assert_eq!(text(&parse_text_runs_at(input, base(), 0.0, 0.0)), "ab");
        assert_eq!(text(&parse_text_runs_at(input, base(), 0.0, 0.99)), "ab");
        assert_eq!(text(&parse_text_runs_at(input, base(), 0.0, 1.0)), "abcd");
        // The `*` form multiplies by the characters shown so far.
        let scaled = "ab<w*0.5>cd";
        assert_eq!(text(&parse_text_runs_at(scaled, base(), 0.0, 0.99)), "ab");
        assert_eq!(text(&parse_text_runs_at(scaled, base(), 0.0, 1.0)), "abcd");
    }

    #[test]
    fn a_clear_hides_the_block_and_starts_the_next_one() {
        let input = "first<c2>second";
        assert_eq!(text(&parse_text_runs_at(input, base(), 0.0, 1.9)), "first");
        assert_eq!(text(&parse_text_runs_at(input, base(), 0.0, 2.0)), "second");
        // `<c>` without a value clears immediately.
        let immediate = "gone<c>now";
        assert_eq!(
            text(&parse_text_runs_at(immediate, base(), 0.0, 0.0)),
            "now"
        );
    }

    #[test]
    fn a_clear_swallows_the_newline_right_after_it() {
        let input = "one<c1>\ntwo";
        assert_eq!(text(&parse_text_runs_at(input, base(), 0.0, 0.0)), "one");
        assert_eq!(text(&parse_text_runs_at(input, base(), 0.0, 1.0)), "two");
    }

    #[test]
    fn timing_and_style_directives_combine() {
        let input = "<r1><#ff0000>ab<#0000ff>cd";
        let runs = parse_text_runs_at(input, base(), 0.0, 2.0);
        assert_eq!(text(&runs), "abc");
        assert_eq!(runs.len(), 2);
        assert_eq!(color(&runs[0]), (0xff, 0x00, 0x00));
        assert_eq!(color(&runs[1]), (0x00, 0x00, 0xff));
        assert_eq!(runs[1].text, "c");
    }

    #[test]
    fn a_base_speed_applies_until_a_directive_changes_it() {
        let runs = parse_text_runs_at("abcd", base(), 1.0, 0.0);
        assert_eq!(text(&runs), "a");
        let reset = parse_text_runs_at("a<r>b", base(), 1.0, 0.0);
        assert_eq!(text(&reset), "a");
        let instant = parse_text_runs_at("a<r0>b", base(), 1.0, 1.0);
        assert_eq!(text(&instant), "ab");
    }

    #[test]
    fn font_directives_change_the_family_and_the_style_switches() {
        let runs = parse_text_runs("<@Meiryo>a<@Meiryo,3BI>b<@+S>c<@-B>d<@>e", base());
        assert_eq!(text(&runs), "abcde");
        assert_eq!(runs.len(), 5);

        assert_eq!(runs[0].font_family.as_deref(), Some("Meiryo"));
        assert!(!runs[0].bold && !runs[0].italic && !runs[0].strikethrough);

        assert_eq!(runs[1].font_family.as_deref(), Some("Meiryo"));
        assert!(runs[1].bold && runs[1].italic && !runs[1].strikethrough);

        // `+`/`-` change only the named styles on the current font.
        assert!(runs[2].strikethrough);
        assert!(runs[2].bold && runs[2].italic);
        assert!(!runs[3].bold);
        assert!(runs[3].strikethrough);

        // `<@>` returns to the object's family and styles.
        assert_eq!(runs[4].font_family, None);
        assert!(!runs[4].bold && !runs[4].strikethrough);
    }

    #[test]
    fn the_extended_size_directive_sets_family_and_style() {
        let runs = parse_text_runs("<s32,Meiryo,BI>a<s>b", base());
        assert_eq!(runs[0].font_size, 32.0);
        assert_eq!(runs[0].font_family.as_deref(), Some("Meiryo"));
        assert!(runs[0].bold && runs[0].italic);
        // `<s>` restores size only; the family and styles keep.
        assert_eq!(runs[1].font_size, 48.0);
        assert_eq!(runs[1].font_family.as_deref(), Some("Meiryo"));
    }

    #[test]
    fn malformed_font_directives_stay_literal() {
        for input in ["<@Meiryo,X>", "<@+Q>", "<@-Bx>", "<@Meiryo,1,2>"] {
            assert_eq!(text(&parse_text_runs(input, base())), input);
        }
    }

    #[test]
    fn ruby_blocks_separate_the_base_text_from_its_furigana() {
        let runs = parse_text_runs("</>漢字<!>かんじ</>", base());
        assert_eq!(text(&runs), "漢字かんじ");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text, "漢字");
        assert_eq!(
            runs[0].ruby,
            Some(RubyRun {
                block: 1,
                base: true,
                scale: 0.5,
                expand: false
            })
        );
        assert_eq!(runs[1].text, "かんじ");
        assert_eq!(
            runs[1].ruby,
            Some(RubyRun {
                block: 1,
                base: false,
                scale: 0.5,
                expand: false
            })
        );

        let runs = parse_text_runs("</>字<!0.4+>じ</>", base());
        let ruby = runs[1].ruby.expect("furigana run");
        assert_eq!(ruby.scale, 0.4);
        assert!(ruby.expand);
        assert!(!ruby.base);

        // `< !>` outside a block and an unclosed block stay readable.
        assert_eq!(text(&parse_text_runs("<!>x</>", base())), "<!>x");
    }

    #[test]
    fn rotation_directives_turn_the_text() {
        let runs = parse_text_runs("<tr90>a<tr+45>b<tr>c", base());
        assert_eq!(runs[0].rotation, 90.0);
        assert_eq!(runs[1].rotation, 135.0);
        assert_eq!(runs[2].rotation, 0.0);
        assert_eq!(text(&parse_text_runs("<trx>a", base())), "<trx>a");
    }

    #[test]
    fn transform_directives_scale_the_glyphs() {
        let runs = parse_text_runs("<tw0.5>a<th2>b<tw>c", base());
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].x_scale, 0.5);
        assert_eq!(runs[0].y_scale, 1.0);
        assert_eq!(runs[1].y_scale, 2.0);
        // `<tw>` restores the horizontal scale only.
        assert_eq!(runs[2].x_scale, 1.0);
        assert_eq!(runs[2].y_scale, 2.0);
        assert_eq!(text(&parse_text_runs("<tx1>a", base())), "<tx1>a");
    }

    #[test]
    fn position_directives_anchor_the_text_that_follows() {
        let runs = parse_text_runs("<p20,40>a", base());
        assert_eq!(runs[0].anchor, TextAnchor::At { x: 20.0, y: 40.0 });

        let runs = parse_text_runs("<p+10,0>a<p+5>b<p>c", base());
        assert_eq!(runs[0].anchor, TextAnchor::At { x: 10.0, y: 0.0 });
        assert_eq!(runs[1].anchor, TextAnchor::At { x: 15.0, y: 0.0 });
        assert_eq!(runs[2].anchor, TextAnchor::Reset);

        // An empty field keeps the previous coordinate.
        let runs = parse_text_runs("<p20>a<p,40>b", base());
        assert_eq!(runs[1].anchor, TextAnchor::At { x: 20.0, y: 40.0 });

        // A malformed position stays literal.
        assert_eq!(text(&parse_text_runs("<px>", base())), "<px>");
        assert_eq!(text(&parse_text_runs("<p1,2,3,4>", base())), "<p1,2,3,4>");
    }

    #[test]
    fn line_spacing_directives_change_the_gap_after_a_line() {
        let runs = parse_text_runs("<gh72>a\nb<gh>c", base());
        assert_eq!(text(&runs), "a\nbc");
        assert_eq!(runs[0].line_spacing, Some(72.0));
        // `<gh>` restores the object's spacing, which is "natural" here.
        assert_eq!(runs[1].line_spacing, None);
        assert_eq!(text(&parse_text_runs("<ghx>", base())), "<ghx>");
    }

    #[test]
    fn comments_and_block_separators_render_nothing() {
        let runs = parse_text_runs("a<// a > sign inside //>b</>c", base());
        assert_eq!(text(&runs), "abc");
        // A comment may span a newline, and it disappears including it.
        let multiline = parse_text_runs("a<// one\ntwo //>b", base());
        assert_eq!(text(&multiline), "ab");
    }
}
