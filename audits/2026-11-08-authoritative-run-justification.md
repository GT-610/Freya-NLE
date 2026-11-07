# Authoritative text runs: justification parity record

Scope: issue #141, the `rasterize_runs` path in
`rust/freya-nle-render/src/text.rs`. Before this change that path left every
line at its natural width, so a justified text object only behaved like
AviUtl when all of its characters shared one style.

## Rule

The single-style path already implements the rule that extra width is shared
between word gaps and that the last line of the block keeps its natural width.
The run path now applies the same rule:

- Only space characters are treated as gaps; a line with no space cannot be
  stretched.
- Gaps are counted across the whole line, so a gap works the same whether it
  sits inside a styled run or on a boundary between runs.
- The extra advance after each gap moves the rest of the line, including later
  runs, their strikethrough rules, and furigana positioned over a base span.
- Lines opened by a `<p...>` anchor keep their explicit x and are not
  justified.
- The last line of the block is left at its natural width. This is the rule
  stated in #141 and the one the single-face path has always followed.
- Other alignments are untouched.

## AviUtl2 comparison

The AviUtl2 binary in the Ghidra project (`AviUtl-reverse`, `/aviutl2.exe`,
14,148 functions) was searched for the text-alignment labels. The only
justification-related UTF-16 label found is `均等配置` ("uniform placement"),
which belongs to an object-placement command, and the text object's alignment
list is left/centre/right plus the vertical variants. The binary is stripped
and this host has no Windows runtime, so a frame-for-frame AviUtl2 capture was
not available.

The comparison therefore used the recorded requirement in #141 — word gaps
distributed, final line natural, consistent with the existing single-face
path — and the implementation deliberately reuses that path's rule rather than
inventing a different distribution. A local render of an authoritative run
block (`alpha beta gamma` / `second line here` / `last line stays`, 48 px,
background on) showed the first two lines sharing the block width and the last
line at its natural width, which is the intended layout.

## Verification

- `justification_spreads_authoritative_runs_across_their_word_gaps`
- `justification_follows_a_gap_across_a_styled_run_boundary`
- `a_line_without_eligible_word_gaps_keeps_its_natural_width`
- `the_last_authoritative_line_keeps_its_natural_width`

`cargo test -p freya-nle-render --lib` passes (58 tests), including the
existing single-face justification and alignment coverage.
