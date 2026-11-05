//! AviUtl text control characters, first batch.
//!
//! The text object accepts inline directives such as `<#ff8800>` or `<s32>`.
//! This module turns one string into styled runs; layout and rasterization stay
//! in the renderer, which receives the runs through [`TextRun`].
//!
//! Supported in this batch:
//!
//! - `<#RRGGBB>` / `<#RRGGBB,RRGGBB>`: text colour, and optionally the outline
//!   colour, with `<#>` restoring the object's colours;
//! - `<sNN>`, `<s+NN>`, `<s-NN>`, `<s*1.5>`: absolute or relative font size,
//!   with `<s>` restoring the object size;
//! - `<gwNN>`: letter spacing, with `<gw>` restoring the object spacing.
//!
//! Anything else between `<` and `>` stays literal text. The font, timing,
//! position, ruby and script directives are later batches, so they are left as
//! text here instead of being silently applied halfway.

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
}

/// The values a run starts from when no directive has changed them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextBaseStyle {
    pub color: RgbaColor,
    pub outline_color: RgbaColor,
    pub font_size: f32,
    pub letter_spacing: f32,
}

const MIN_FONT_SIZE: f32 = 1.0;
const MAX_FONT_SIZE: f32 = 4_096.0;
const MAX_LETTER_SPACING: f32 = 1_000.0;

/// Splits one text string into styled runs.
///
/// A directive that is not understood is kept as literal text, which is what
/// AviUtl does for a mistyped control character. Adjacent text with the same
/// style stays one run, so a plain string yields exactly one.
pub fn parse_text_runs(input: &str, base: TextBaseStyle) -> Vec<TextRun> {
    let mut runs = Vec::new();
    let mut style = base;
    let mut text_start = 0;
    let mut index = 0;
    while index < input.len() {
        if input.as_bytes()[index] == b'<' {
            let Some(relative_end) = input[index + 1..].find('>') else {
                break;
            };
            let directive_end = index + 1 + relative_end;
            let directive = &input[index + 1..directive_end];
            if let Some(next) = apply_directive(directive, style, base) {
                push_run(&mut runs, &input[text_start..index], style);
                style = next;
                index = directive_end + 1;
                text_start = index;
                continue;
            }
        }
        // Advance one whole character; the scan above only compares ASCII '<'
        // and '>', so a multi-byte character never starts a directive.
        let width = input[index..].chars().next().map_or(1, char::len_utf8);
        index += width;
    }
    push_run(&mut runs, &input[text_start..], style);
    runs
}

fn push_run(runs: &mut Vec<TextRun>, text: &str, style: TextBaseStyle) {
    if text.is_empty() {
        return;
    }
    runs.push(TextRun {
        text: text.to_owned(),
        color: style.color,
        outline_color: style.outline_color,
        font_size: style.font_size,
        letter_spacing: style.letter_spacing,
    });
}

fn apply_directive(
    directive: &str,
    current: TextBaseStyle,
    base: TextBaseStyle,
) -> Option<TextBaseStyle> {
    let mut characters = directive.chars();
    match characters.next()? {
        '#' => apply_color_directive(&directive[1..], current, base),
        's' => apply_size_directive(&directive[1..], current, base),
        'g' => apply_spacing_directive(&directive[1..], current, base),
        _ => None,
    }
}

fn apply_color_directive(
    value: &str,
    current: TextBaseStyle,
    base: TextBaseStyle,
) -> Option<TextBaseStyle> {
    let mut parts = value.split(',');
    let text = parts.next().unwrap_or_default();
    let outline = parts.next();
    if parts.next().is_some() {
        return None;
    }
    let color = if text.is_empty() {
        base.color
    } else {
        parse_color(text)?
    };
    // A single colour changes the text only; the outline keeps whatever the
    // previous directive left, matching AviUtl's `<#文字色,影縁色>` form.
    let outline_color = match outline {
        None => current.outline_color,
        Some(value) if value.trim().is_empty() => base.outline_color,
        Some(value) => parse_color(value)?,
    };
    Some(TextBaseStyle {
        color,
        outline_color,
        ..current
    })
}

fn apply_size_directive(
    value: &str,
    current: TextBaseStyle,
    base: TextBaseStyle,
) -> Option<TextBaseStyle> {
    // `<s32,フォント,スタイル,縁取り>` changes several things at once; this
    // batch owns the size and leaves the rest for the font batch.
    let size = value.split(',').next().unwrap_or_default().trim();
    let font_size = if size.is_empty() {
        base.font_size
    } else {
        resolve_size(size, current.font_size)?
    };
    Some(TextBaseStyle {
        font_size: font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE),
        ..current
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

fn apply_spacing_directive(
    value: &str,
    current: TextBaseStyle,
    base: TextBaseStyle,
) -> Option<TextBaseStyle> {
    // `<gh>` (line spacing) has no per-run meaning, so only `<gw>` is handled.
    let spacing = value.strip_prefix('w')?.trim();
    let letter_spacing = if spacing.is_empty() {
        base.letter_spacing
    } else {
        let value = spacing.parse::<f32>().ok()?;
        if !value.is_finite() {
            return None;
        }
        value.clamp(-MAX_LETTER_SPACING, MAX_LETTER_SPACING)
    };
    Some(TextBaseStyle {
        letter_spacing,
        ..current
    })
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
        }
    }

    fn color(run: &TextRun) -> (u8, u8, u8) {
        (run.color.red, run.color.green, run.color.blue)
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
        // The unknown directive is part of the text, not swallowed.
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
}
