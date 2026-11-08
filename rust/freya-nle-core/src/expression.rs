//! Evaluates AviUtl-style `=expression` parameter values.
//!
//! A parameter whose stored value is a string beginning with `=` is a Lua
//! expression instead of a literal. It is evaluated with `time` (seconds from
//! the start of the object) and `value` (the keyframe-resolved value for the
//! same parameter) in scope, matching what the Qt frontend's expression host
//! exposed. The evaluator is lazy: a project without expressions never pays for
//! a Lua state.
//!
//! Text objects can also carry `<?...?>` blocks: the script runs for the frame
//! and the text it prints with `mes(value)` replaces the block, matching
//! AviUtl's text scripts (`<?=expression?>` is the shorthand AviUtl2 rewrites
//! into `mes(expression)`). Only read-only scripts are supported — `obj`
//! exposes the frame clock, and assignments to it have no effect on the
//! object.

use luna_core::runtime::Value;
use luna_core::version::LuaVersion;
use luna_core::vm::{HostRootTicket, Vm};
use std::cell::RefCell;
use std::collections::HashMap;

/// The Qt host rejected anything longer before it reached the parser.
/// Longest expression the evaluator accepts. The edit path checks the same
/// bound so a stored expression is always one the renderer can run.
pub(crate) const MAX_EXPRESSION_BYTES: usize = 10_000;

const MAX_EXPRESSION_LENGTH: usize = MAX_EXPRESSION_BYTES;
/// Compiled expressions are retained per thread, so the cache is bounded.
const MAX_COMPILED_EXPRESSIONS: usize = 1_000;
const INSTRUCTION_BUDGET: i64 = 100_000;
const MEMORY_CAP: usize = 16 * 1024 * 1024;
const LOADER_INPUT_BUDGET: usize = 64 * 1024;

thread_local! {
    static EVALUATOR: RefCell<Option<Evaluator>> = const { RefCell::new(None) };
    static TEXT_SCRIPTS: RefCell<Option<TextScriptVm>> = const { RefCell::new(None) };
}

/// The object transform a text script may read and write.
///
/// AviUtl's `obj.ox/oy/oz`, `rx/ry/rz`, `zoom` and `alpha` are writable, so a
/// script such as `<?obj.rz = obj.time * 360?>` rotates the object. The values
/// start from the object's own transform for this frame and are applied back
/// after every block has run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TextScriptTransform {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub rotation_x: f32,
    pub rotation_y: f32,
    pub rotation_z: f32,
    pub scale_percent: f32,
    pub opacity: f32,
    /// Aspect ratio (`obj.aspect`), `-1.0..=1.0`: positive squeezes
    /// horizontally, negative squeezes vertically, 0 is unchanged. The same
    /// units the compositor's `LayerTransform` computes with.
    pub aspect: f32,
    /// Pivot the object rotates around.
    pub pivot_x: f32,
    pub pivot_y: f32,
    pub pivot_z: f32,
}

/// The clock and object facts a text script sees for the frame being rendered.
pub(crate) struct TextScriptClock {
    /// Seconds from the start of the object.
    pub time: f64,
    /// Length of the object in seconds.
    pub total_time: f64,
    pub frame: i32,
    pub total_frames: i32,
    pub frame_rate: f64,
    /// Layer the text object sits on; also AviUtl's `obj.effect_layer`.
    pub layer: i32,
    /// Scene-based start and end frame of the object (`obj.frame_s`/`frame_e`).
    pub start_frame: i32,
    pub end_frame: i32,
    pub screen_width: i32,
    pub screen_height: i32,
    pub scene_id: i32,
    /// The clip's document id, which a script sees as `obj.id` and
    /// `obj.effect_id`; AviUtl2's ids are per launch, ours are per project.
    pub clip_id: i32,
    /// The object's own transform, which a script may change.
    pub transform: TextScriptTransform,
}

/// Replaces every `<?...?>` block with what its script prints.
///
/// A block that fails to parse or run keeps its original text, so the author
/// can see which directive did not work instead of a silent gap. The second
/// value is the transform the scripts left behind, or `None` when no block ran
/// or none of them changed it.
pub(crate) fn expand_text_scripts(
    content: &str,
    clock: &TextScriptClock,
) -> (String, Option<TextScriptTransform>) {
    if !content.contains("<?") {
        return (content.to_owned(), None);
    }
    TEXT_SCRIPTS.with(|slot| {
        let mut slot = slot.borrow_mut();
        let vm = slot.get_or_insert_with(TextScriptVm::new);
        vm.begin(clock);
        let mut output = String::with_capacity(content.len());
        let mut ran = false;
        let mut truncated = false;
        let mut rest = content;
        while let Some(start) = rest.find("<?") {
            output.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            let Some(end) = after.find("?>") else {
                output.push_str(&rest[start..]);
                truncated = true;
                break;
            };
            let block = &after[..end];
            match vm.evaluate_block(block) {
                Some(value) => {
                    ran = true;
                    output.push_str(&value);
                }
                None => output.push_str(&rest[start..start + 2 + end + 2]),
            }
            rest = &after[end + 2..];
        }
        if !truncated {
            output.push_str(rest);
        }
        // Only report a transform the script actually changed: a block that
        // merely prints must not freeze the object's values for this frame.
        let transform = if ran {
            vm.finish(clock)
                .filter(|transform| *transform != clock.transform)
        } else {
            None
        };
        (output, transform)
    })
}

struct TextScriptVm {
    vm: Vm,
    /// Captured before the script environment hides them: each block needs them
    /// to install and compile its environment, and a script must not reach them.
    setmetatable: Value,
    setfenv: Value,
    /// `loadstring` is removed from the globals so a block cannot reach it, but
    /// the captured `Value` alone is not a GC root. Pinning it keeps the native
    /// closure alive for every later block on this thread.
    load: Value,
    load_ticket: HostRootTicket,
}

/// Runs one block in a fresh environment.
///
/// The wrapper receives the functions it needs as chunk arguments, so they can
/// be hidden from the block, and compiles the block itself as a separate chunk:
/// a chunk shares no locals with its loader, so the block cannot reach the
/// wrapper's environment or helpers. `__env` reads through `_G`, whose library
/// tables are guarded proxies (see `TextScriptVm::new`). The `__index` handler
/// rejects the escape hatches by name, so a block cannot reach them even by
/// removing a shadowed key (`_G = nil`) and falling through to the real global
/// table; the shadowed `false` entries are a first line of defence only.
const TEXT_SCRIPT_WRAPPER: &str = "\
local __setmetatable, __setfenv, __load, __source = ...\n\
local __env = __setmetatable({}, {\n\
  __index = function(_, key)\n\
    if key == \"_G\" or key == \"setmetatable\" or key == \"getmetatable\"\n\
       or key == \"setfenv\" or key == \"getfenv\" or key == \"rawset\" or key == \"rawget\" then\n\
      return nil\n\
    end\n\
    return _G[key]\n\
  end\n\
})\n\
__setfenv(1, __env)\n\
_G = false\n\
setmetatable = false\n\
getmetatable = false\n\
setfenv = false\n\
getfenv = false\n\
rawset = false\n\
rawget = false\n\
local __chunk = __load(__source)\n\
__setfenv(__chunk, __env)\n\
local __collector = __collector\n\
local __concat = table.concat\n\
__collector.items = {}\n\
__chunk()\n\
local out = __concat(__collector.items)\n\
__collector.items = {}\n\
return out\n";

impl TextScriptVm {
    fn new() -> Self {
        let mut vm = Vm::sandbox(LuaVersion::Lua51)
            .open_base()
            .open_math()
            .open_string()
            .open_table()
            .with_instr_budget(INSTRUCTION_BUDGET)
            .with_memory_cap(MEMORY_CAP)
            .build();
        vm.set_loader_input_budget(LOADER_INPUT_BUDGET);
        let setmetatable = table_global(&mut vm, "setmetatable");
        let setfenv = table_global(&mut vm, "setfenv");
        let load = table_global(&mut vm, "loadstring");
        for name in [
            "load",
            "loadstring",
            "loadfile",
            "dofile",
            "io",
            "os",
            "debug",
            "package",
            "ffi",
        ] {
            let _ = vm.set_global(name, ());
        }
        // `setfenv` isolates assignments and reads per block, but a script can
        // still mutate a table it reaches through a shared global: `math.sin`
        // lives on the one `math` table every text clip on this thread sees.
        // Give each library an empty proxy whose metatable reads the real table
        // and sends every write to a discard. Reads behave exactly as before
        // and no write can reach the shared library, without cloning the tables
        // per object or per frame.
        let _ = vm.eval_chunk(
            "local function __discard() end\n\
             local function __guard(real)\n\
             return setmetatable({}, { __index = real, __newindex = __discard })\n\
             end\n\
             math = __guard(math)\n\
             string = __guard(string)\n\
             table = __guard(table)\n\
             m = math\n\
             sin = math.sin; cos = math.cos; tan = math.tan; abs = math.abs; \n\
             max = math.max; min = math.min; floor = math.floor; ceil = math.ceil; \n\
             pi = math.pi; random = math.random;",
            "=text-script-libraries",
        );
        let load_ticket = vm.pin_host(load);
        Self {
            vm,
            setmetatable,
            setfenv,
            load,
            load_ticket,
        }
    }

    /// Builds the `obj` table for this frame and resets the print collector.
    ///
    /// Every block of one object shares this table, so a write in the first
    /// block is visible to the next, the way one script running per frame sees
    /// its own earlier statements.
    fn begin(&mut self, clock: &TextScriptClock) {
        let _ = self.vm.set_global("time", clock.time);
        let _ = self.vm.set_global("totaltime", clock.total_time);
        let _ = self.vm.set_global("frame", i64::from(clock.frame));
        let _ = self
            .vm
            .set_global("totalframe", i64::from(clock.total_frames));
        let _ = self.vm.set_global("framerate", clock.frame_rate);
        let transform = clock.transform;
        let object = self
            .vm
            .new_table()
            .with("time", clock.time)
            .with("totaltime", clock.total_time)
            .with("frame", i64::from(clock.frame))
            .with("totalframe", i64::from(clock.total_frames))
            .with("framerate", clock.frame_rate)
            .with("layer", i64::from(clock.layer))
            .with("effect_layer", i64::from(clock.layer))
            .with("frame_s", i64::from(clock.start_frame))
            .with("frame_e", i64::from(clock.end_frame))
            // Text renders from the scene's first frame, so the render origin
            // is always zero here.
            .with("originframe", 0_i64)
            .with("screen_w", i64::from(clock.screen_width))
            .with("screen_h", i64::from(clock.screen_height))
            .with("scene_id", i64::from(clock.scene_id))
            .with("id", i64::from(clock.clip_id))
            .with("effect_id", i64::from(clock.clip_id))
            // Individual objects are not modelled, so a text renders as one:
            // AviUtl2 reports index 1 and num 1 for a single object too.
            .with("index", 1_i64)
            .with("num", 1_i64)
            // Writable transform fields, seeded from the object's own values.
            .with("ox", transform.x)
            .with("oy", transform.y)
            .with("oz", transform.z)
            .with("x", transform.x)
            .with("y", transform.y)
            .with("z", transform.z)
            .with("rx", transform.rotation_x)
            .with("ry", transform.rotation_y)
            .with("rz", transform.rotation_z)
            .with("zoom", transform.scale_percent / 100.0)
            .with("alpha", transform.opacity)
            .with("aspect", transform.aspect)
            .with("cx", transform.pivot_x)
            .with("cy", transform.pivot_y)
            .with("cz", transform.pivot_z)
            .build();
        let _ = self.vm.set_global("obj", Value::Table(object));
        // `mes` must collect per object, not per frame, so the collector is
        // rebuilt here instead of living in the prelude. A Lua function cannot
        // carry fields, so the buffer is a separate table.
        let _ = self.vm.eval_chunk(
            "__collector = { items = {} }\n\
             mes = function(value)\n\
             if value ~= nil then\n\
             __collector.items[#__collector.items + 1] = tostring(value)\n\
             end\n\
             end\n\
             obj.mes = mes",
            "=text-script-collector",
        );
    }

    /// Runs one `<?...?>` block and returns what it printed.
    fn evaluate_block(&mut self, block: &str) -> Option<String> {
        let block = block.trim();
        // AviUtl2 removes the `=` and wraps the rest in `mes(...)`, so both
        // forms become one statement chunk.
        let script = match block.strip_prefix('=') {
            Some(expression) => format!("mes({})", expression.trim()),
            None => block.to_owned(),
        };
        if script.len() > MAX_EXPRESSION_LENGTH {
            return None;
        }
        // The collector is cleared at the top of the chunk, not only after a
        // successful return: a block that prints and then fails would otherwise
        // leave its text for the next block in the same object to print ahead
        // of its own output.
        let wrapper = self
            .vm
            .load(TEXT_SCRIPT_WRAPPER.as_bytes(), b"=text-script")
            .ok()?;
        let source = Value::Str(self.vm.intern_str(&script));
        // The budget is per call, not per VM: this VM is reused for every text
        // object on the thread, so without re-arming it the allowance would be
        // spent by earlier scripts until a later one fails and stays literal.
        self.vm.set_instr_budget(Some(INSTRUCTION_BUDGET));
        let values = self
            .vm
            .call_value(
                Value::Closure(wrapper),
                &[
                    self.setmetatable,
                    self.setfenv,
                    self.vm.read_host(self.load_ticket).unwrap_or(self.load),
                    source,
                ],
            )
            .ok()?;
        // The chunk always returns a string; a block that printed nothing
        // yields the empty string.
        Some(match values.first()? {
            Value::Str(value) => String::from_utf8_lossy(value.as_bytes()).into_owned(),
            _ => String::new(),
        })
    }

    /// Reads the writable fields back after the blocks ran.
    fn finish(&mut self, clock: &TextScriptClock) -> Option<TextScriptTransform> {
        let object_key = Value::Str(self.vm.intern_str("obj"));
        let object = match self.vm.globals().get(object_key) {
            Value::Table(table) => table,
            _ => return None,
        };
        let mut read = |name: &str| -> Option<f32> {
            let key = Value::Str(self.vm.intern_str(name));
            match object.get(key) {
                Value::Int(value) => Some(value as f32),
                Value::Float(value) => Some(value as f32),
                _ => None,
            }
        };
        let transform = TextScriptTransform {
            x: read("ox").unwrap_or(clock.transform.x),
            y: read("oy").unwrap_or(clock.transform.y),
            z: read("oz").unwrap_or(clock.transform.z),
            rotation_x: read("rx").unwrap_or(clock.transform.rotation_x),
            rotation_y: read("ry").unwrap_or(clock.transform.rotation_y),
            rotation_z: read("rz").unwrap_or(clock.transform.rotation_z),
            scale_percent: read("zoom").map_or(clock.transform.scale_percent, |zoom| zoom * 100.0),
            opacity: read("alpha").unwrap_or(clock.transform.opacity),
            // AviUtl2 documents `obj.aspect` as `-1.0..=1.0`. The compositor
            // multiplies by `1.0 + aspect`, so an unclamped 100 (a percentage
            // written by an older script) would stretch the object 101x.
            aspect: read("aspect")
                .unwrap_or(clock.transform.aspect)
                .clamp(-1.0, 1.0),
            pivot_x: read("cx").unwrap_or(clock.transform.pivot_x),
            pivot_y: read("cy").unwrap_or(clock.transform.pivot_y),
            pivot_z: read("cz").unwrap_or(clock.transform.pivot_z),
        };
        // A script that wrote nonsense leaves the object where it was rather
        // than making it invisible or infinitely large.
        let finite = [
            transform.x,
            transform.y,
            transform.z,
            transform.rotation_x,
            transform.rotation_y,
            transform.rotation_z,
            transform.scale_percent,
            transform.opacity,
            transform.aspect,
            transform.pivot_x,
            transform.pivot_y,
            transform.pivot_z,
        ]
        .into_iter()
        .all(f32::is_finite);
        finite.then_some(transform)
    }
}

/// Whether a stored parameter value is an expression rather than a literal.
pub(crate) fn is_expression(value: &str) -> bool {
    value.starts_with('=')
}

/// Evaluates `expression` (without its leading `=`) and returns its numeric
/// result. A parse failure, a runtime error, a non-numeric result, or an
/// over-long expression all yield `None`, so the caller keeps the literal
/// fallback it already resolved.
pub(crate) fn evaluate(
    expression: &str,
    time_seconds: f64,
    index: i32,
    current_value: f64,
) -> Option<f64> {
    if expression.len() > MAX_EXPRESSION_LENGTH {
        return None;
    }
    EVALUATOR.with(|slot| {
        let mut slot = slot.borrow_mut();
        let evaluator = slot.get_or_insert_with(Evaluator::new);
        evaluator.evaluate(expression, time_seconds, index, current_value)
    })
}

struct Evaluator {
    vm: Vm,
    /// `None` records a source that failed to parse, so it is not retried on
    /// every frame.
    compiled: HashMap<String, Option<HostRootTicket>>,
}

impl Evaluator {
    fn new() -> Self {
        let mut vm = Vm::sandbox(LuaVersion::Lua51)
            .open_base()
            .open_math()
            .open_string()
            .open_table()
            .with_instr_budget(INSTRUCTION_BUDGET)
            .with_memory_cap(MEMORY_CAP)
            .build();
        vm.set_loader_input_budget(LOADER_INPUT_BUDGET);
        // The sandbox keeps the same safe surface the Qt expression host had:
        // no loader, no io/os/debug, and the math alias plus shortcuts.
        for name in [
            "load",
            "loadstring",
            "loadfile",
            "dofile",
            "io",
            "os",
            "debug",
            "package",
            "ffi",
        ] {
            let _ = vm.set_global(name, ());
        }
        let math = table_global(&mut vm, "math");
        let _ = vm.set_global("m", math);
        let _ = vm.eval_chunk(
            "sin = math.sin; cos = math.cos; tan = math.tan; abs = math.abs; \
             max = math.max; min = math.min; floor = math.floor; ceil = math.ceil; \
             pi = math.pi; random = math.random; \
             clamp = function(value, low, high) if value < low then return low \
             elseif value > high then return high else return value end end;",
            "=expression-shortcuts",
        );
        Self {
            vm,
            compiled: HashMap::new(),
        }
    }

    fn evaluate(
        &mut self,
        expression: &str,
        time_seconds: f64,
        index: i32,
        current_value: f64,
    ) -> Option<f64> {
        let cached = self.compiled.get(expression).copied();
        let ticket = match cached {
            Some(Some(ticket)) => Some(ticket),
            Some(None) => None,
            None => {
                if self.compiled.len() >= MAX_COMPILED_EXPRESSIONS {
                    return None;
                }
                let ticket = self.compile(expression);
                self.compiled.insert(expression.to_owned(), ticket);
                ticket
            }
        };
        let function = ticket.and_then(|ticket| self.vm.read_host(ticket))?;
        self.vm.set_instr_budget(Some(INSTRUCTION_BUDGET));
        let _ = self.vm.set_global("time", time_seconds);
        let _ = self.vm.set_global("t", time_seconds);
        let _ = self.vm.set_global("index", i64::from(index));
        let _ = self.vm.set_global("value", current_value);
        let _ = self.vm.set_global("v", current_value);
        match self.vm.call_value(function, &[]) {
            Ok(values) => match values.first() {
                Some(Value::Float(value)) => Some(*value),
                Some(Value::Int(value)) => Some(*value as f64),
                _ => None,
            },
            Err(_) => None,
        }
    }

    fn compile(&mut self, expression: &str) -> Option<HostRootTicket> {
        let source = format!("return {expression}");
        let closure = self.vm.load(source.as_bytes(), b"=expression").ok()?;
        Some(self.vm.pin_host(Value::Closure(closure)))
    }
}

fn table_global(vm: &mut Vm, name: &str) -> Value {
    let key = Value::Str(vm.intern_str(name));
    vm.globals().get(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluates_arithmetic_with_time_and_value_in_scope() {
        assert_eq!(evaluate("1 + 2", 0.0, 0, 0.0), Some(3.0));
        assert_eq!(evaluate("value * 2", 0.0, 0, 5.0), Some(10.0));
        assert_eq!(evaluate("v + 1", 0.0, 0, 5.0), Some(6.0));
        assert_eq!(evaluate("time * 60", 1.5, 0, 0.0), Some(90.0));
        assert_eq!(evaluate("t", 0.25, 0, 0.0), Some(0.25));
        assert_eq!(evaluate("index + 1", 0.0, 4, 0.0), Some(5.0));
        assert_eq!(evaluate("sin(0)", 0.0, 0, 0.0), Some(0.0));
        assert_eq!(evaluate("m.floor(2.9)", 0.0, 0, 0.0), Some(2.0));
        assert_eq!(evaluate("math.max(2, 7)", 0.0, 0, 0.0), Some(7.0));
    }

    #[test]
    fn rejects_broken_or_non_numeric_expressions_without_panicking() {
        assert_eq!(evaluate("1 +", 0.0, 0, 0.0), None);
        assert_eq!(evaluate("\"text\"", 0.0, 0, 0.0), None);
        assert_eq!(evaluate("nil", 0.0, 0, 0.0), None);
        assert_eq!(evaluate("error('boom')", 0.0, 0, 0.0), None);
        assert_eq!(evaluate(&"1+".repeat(4_000), 0.0, 0, 0.0), None);
    }

    #[test]
    fn expression_prefix_detection_matches_the_stored_form() {
        assert!(is_expression("=time*10"));
        assert!(!is_expression("10"));
        assert!(!is_expression(""));
        assert!(!is_expression("#ffffff"));
    }

    fn clock(time: f64, total_time: f64) -> TextScriptClock {
        TextScriptClock {
            time,
            total_time,
            frame: (time * 30.0) as i32,
            total_frames: (total_time * 30.0) as i32,
            frame_rate: 30.0,
            layer: 0,
            start_frame: 0,
            end_frame: (total_time * 30.0) as i32,
            screen_width: 1920,
            screen_height: 1080,
            scene_id: 1,
            clip_id: 7,
            transform: TextScriptTransform {
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
            },
        }
    }

    /// The text of one expansion, which is what most tests care about.
    fn text(content: &str, clock: &TextScriptClock) -> String {
        expand_text_scripts(content, clock).0
    }

    #[test]
    fn text_scripts_print_through_mes_like_aviutl() {
        let clock = clock(1.5, 12.0);
        assert_eq!(text("a<?=4+8?>b", &clock), "a12b");
        assert_eq!(text("<?=string.format(\"%02d\", 5)?>", &clock), "05");
        assert_eq!(text("<? mes(obj.time) ?>", &clock), "1.5");
        assert_eq!(
            text("<? t=obj.time mes(string.format(\"%.1f\", t)) ?>", &clock),
            "1.5"
        );
        assert_eq!(text("<? mes(\"a\") mes(\"b\") ?>", &clock), "ab");
        assert_eq!(
            text("<? mes(totaltime) ?>/<? mes(totalframe) ?>", &clock),
            "12/360"
        );
        assert_eq!(
            text("<? obj.mes(string.format(\"%d\", obj.frame)) ?>", &clock),
            "45"
        );
        assert_eq!(
            text(
                "<? mes(obj.layer) ?>/<? mes(obj.frame_s) ?>/<? mes(obj.frame_e) ?>",
                &clock
            ),
            "0/0/360"
        );
        assert_eq!(
            text(
                "<? mes(obj.screen_w) ?>x<? mes(obj.screen_h) ?>/<? mes(obj.scene_id) ?>",
                &clock
            ),
            "1920x1080/1"
        );
        // Individual objects are not modelled, so the object reports itself as
        // the single one, with the clip id as both ids.
        assert_eq!(
            text(
                "<? mes(obj.id) ?>/<? mes(obj.effect_id) ?>/<? mes(obj.index) ?>/<? mes(obj.num) ?>",
                &clock
            ),
            "7/7/1/1"
        );
    }

    #[test]
    fn a_text_script_can_write_the_object_transform() {
        let mut clock = clock(1.5, 12.0);
        clock.transform = TextScriptTransform {
            x: 100.0,
            y: 50.0,
            z: 0.0,
            rotation_x: 0.0,
            rotation_y: 0.0,
            rotation_z: 10.0,
            scale_percent: 100.0,
            opacity: 1.0,
            aspect: 0.0,
            pivot_x: 0.0,
            pivot_y: 0.0,
            pivot_z: 0.0,
        };
        // AviUtl's documented example rotates the object by the elapsed time.
        let (printed, transform) = expand_text_scripts("<?obj.rz=obj.time*360?>", &clock);
        assert_eq!(printed, "");
        let transform = transform.expect("the script changed the transform");
        assert_eq!(transform.rotation_z, 540.0);
        assert_eq!(transform.x, 100.0, "untouched fields keep their value");

        // The same object's later blocks see the write.
        let (printed, transform) =
            expand_text_scripts("<?obj.rz=90?><?obj.rz=obj.rz+10?><?=obj.rz?>", &clock);
        assert_eq!(printed, "100");
        assert_eq!(
            transform.expect("changed").rotation_z,
            100.0,
            "writes accumulate across blocks"
        );

        // Reading a writable field without writing it reports no change, so a
        // script that only prints does not freeze the object.
        let (printed, transform) = expand_text_scripts("<?=obj.zoom?>", &clock);
        assert_eq!(printed, "1");
        assert_eq!(transform, None);

        // A non-numeric write stays in the table but is not applied: the
        // object keeps its own opacity instead of vanishing.
        let (printed, transform) = expand_text_scripts("<?obj.alpha='x'?><?=obj.alpha?>", &clock);
        assert_eq!(printed, "x", "the script sees what it wrote");
        assert_eq!(transform, None, "a non-numeric value is not applied");
        // `zoom` is a multiplier of the object's scale.
        let (_, transform) = expand_text_scripts("<?obj.zoom=2?>", &clock);
        assert_eq!(transform.expect("changed").scale_percent, 200.0);
        // AviUtl marks `aspect` and the pivot (`cx`/`cy`/`cz`) writable too,
        // in the documented `-1.0..=1.0` range.
        let (printed, transform) =
            expand_text_scripts("<?obj.aspect=0.5?><?obj.cx=32?><?=obj.aspect?>", &clock);
        assert_eq!(printed, "0.5");
        let transform = transform.expect("aspect and pivot changed");
        assert_eq!(transform.aspect, 0.5);
        assert_eq!(transform.pivot_x, 32.0);
        assert_eq!(transform.pivot_y, 0.0, "an untouched pivot keeps its value");
    }

    /// `aspect` is a ratio, not a percentage: the compositor stretches by
    /// `1.0 + aspect`, so a script written against the old percentage
    /// convention has to be clamped instead of blowing the object up 101x.
    #[test]
    fn a_script_written_aspect_is_clamped_to_the_documented_range() {
        let clock = clock(0.0, 1.0);
        for (script, expected) in [
            ("<?obj.aspect=100?>", 1.0),
            ("<?obj.aspect=150?>", 1.0),
            ("<?obj.aspect=-100?>", -1.0),
            ("<?obj.aspect=0.25?>", 0.25),
            ("<?obj.aspect=-0.5?>", -0.5),
        ] {
            let (_, transform) = expand_text_scripts(script, &clock);
            let transform = transform.expect("the script wrote aspect");
            assert_eq!(transform.aspect, expected, "{script}");
        }
        // Leaving it alone keeps the object's own value rather than clamping it
        // into range, so an untouched object is not silently normalised.
        let (_, transform) = expand_text_scripts("<?obj.cx=5?>", &clock);
        assert_eq!(transform.expect("pivot changed").aspect, 0.0);
    }

    /// A block that prints and then fails must not leave its text behind for
    /// the next block in the same object to print ahead of its own output.
    #[test]
    fn a_failing_block_does_not_leak_its_output_into_the_next_one() {
        let clock = clock(1.0, 1.0);
        let expanded = text(
            "<? mes(\"leaked\") error('boom') ?>|<? mes(\"kept\") ?>",
            &clock,
        );
        assert!(
            !expanded.contains("leakedkept") && !expanded.starts_with("leaked"),
            "the failed block's output must not run into the next block: {expanded}"
        );
        assert!(expanded.contains("kept"), "{expanded}");
    }

    #[test]
    fn a_failed_text_script_keeps_its_literal_block() {
        let clock = clock(0.0, 1.0);
        assert_eq!(text("x<?=1+?>y", &clock), "x<?=1+?>y");
        assert_eq!(
            text("x<? error('boom') ?>y", &clock),
            "x<? error('boom') ?>y"
        );
        assert_eq!(text("plain text", &clock), "plain text");
        assert_eq!(text("dangling <?x", &clock), "dangling <?x");
    }

    #[test]
    fn a_text_script_cannot_poison_a_shared_library_table() {
        let first = clock(0.0, 1.0);
        let second = clock(1.0, 2.0);
        // Replacing an existing key must not reach the table the next clip on
        // this thread reads, on a later frame or after the object changes.
        assert_eq!(
            text(
                "<? math.floor = function() return 0 end ?><?=math.floor(2.9)?>",
                &first
            ),
            "2"
        );
        assert_eq!(text("<?=math.floor(2.9)?>", &second), "2");
        // The `m` alias and the shortcut globals read the same guarded table;
        // writing through either must be discarded.
        assert_eq!(text("<? m.floor = function() return 0 end ?>", &first), "");
        assert_eq!(text("<?=m.floor(2.9)?>/<?=floor(2.9)?>", &second), "2/2");
        // The other exposed library tables are guarded the same way.
        assert_eq!(
            text("<? string.rep = function() return 'x' end ?>", &first),
            ""
        );
        assert_eq!(text("<?=string.rep('a', 2)?>", &second), "aa");
        assert_eq!(
            text("<? table.concat = function() return 'x' end ?>", &first),
            ""
        );
        assert_eq!(text("<?=table.concat({'a','b'})?>", &second), "ab");
    }

    #[test]
    fn a_text_script_cannot_bypass_the_guard_through_the_environment() {
        let clock = clock(0.0, 1.0);
        // The escape hatches are shadowed inside the block, so mutating the
        // proxy directly fails and keeps the block literal instead of reaching
        // the real table.
        assert_eq!(
            text(
                "<? rawset(math, 'floor', function() return 0 end) ?>",
                &clock
            ),
            "<? rawset(math, 'floor', function() return 0 end) ?>"
        );
        assert_eq!(text("<?=math.floor(2.9)?>", &clock), "2");
        assert_eq!(
            text("<? setmetatable(math, {}) ?>", &clock),
            "<? setmetatable(math, {}) ?>"
        );
        assert_eq!(text("<?=math.floor(2.9)?>", &clock), "2");
        assert_eq!(
            text("<? _G.math.floor = function() return 0 end ?>", &clock),
            "<? _G.math.floor = function() return 0 end ?>"
        );
        assert_eq!(text("<?=math.floor(2.9)?>", &clock), "2");
        // The loader's own locals are out of reach because the block is a
        // separate chunk, not interpolated into the wrapper.
        assert_eq!(
            text("<? __setmetatable(math, {}) ?>", &clock),
            "<? __setmetatable(math, {}) ?>"
        );
        assert_eq!(text("<?=math.floor(2.9)?>", &clock), "2");
        // Ordinary reads still work, including the string library's methods.
        assert_eq!(text("<?=string.format('%02d', 5)?>", &clock), "05");
        assert_eq!(text("<?=('ab'):upper()?>", &clock), "AB");
    }

    #[test]
    fn a_text_script_cannot_reach_the_globals_by_removing_a_shadow() {
        let clock = clock(0.0, 1.0);
        // Removing the `_G` shadow must not expose the shared global table. Because
        // each block gets a fresh environment, the removal and the lookup have to
        // share one block for the assertion to mean anything: without the
        // `__index` rejection, `_G.math = {}` would reach the real table.
        assert_eq!(
            text("<? _G = nil; _G.math = {} ?>", &clock),
            "<? _G = nil; _G.math = {} ?>"
        );
        assert_eq!(text("<?=math.floor(2.9)?>", &clock), "2");
        // The same holds for setmetatable once its shadow is removed.
        assert_eq!(
            text("<? setmetatable = nil; setmetatable(math, {}) ?>", &clock),
            "<? setmetatable = nil; setmetatable(math, {}) ?>"
        );
        assert_eq!(text("<?=math.floor(2.9)?>", &clock), "2");
    }

    #[test]
    fn each_text_script_runs_in_its_own_environment() {
        let clock = clock(0.0, 1.0);
        // `c` is assigned by the first block; the second block must not see it,
        // so `mes(c)` prints nothing.
        assert_eq!(text("<? c = 7 ?><?=c?>", &clock), "");
        // Re-evaluating does not accumulate state in the shared VM.
        assert_eq!(text("<?=c?>", &clock), "");
    }
}
