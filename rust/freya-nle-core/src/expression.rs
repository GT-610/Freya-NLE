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
const MAX_EXPRESSION_LENGTH: usize = 10_000;
/// Compiled expressions are retained per thread, so the cache is bounded.
const MAX_COMPILED_EXPRESSIONS: usize = 1_000;
const INSTRUCTION_BUDGET: i64 = 100_000;
const MEMORY_CAP: usize = 16 * 1024 * 1024;
const LOADER_INPUT_BUDGET: usize = 64 * 1024;

thread_local! {
    static EVALUATOR: RefCell<Option<Evaluator>> = const { RefCell::new(None) };
    static TEXT_SCRIPTS: RefCell<Option<TextScriptVm>> = const { RefCell::new(None) };
}

/// The clock a text script sees for the frame being rendered.
pub(crate) struct TextScriptClock {
    /// Seconds from the start of the object.
    pub time: f64,
    /// Length of the object in seconds.
    pub total_time: f64,
    pub frame: i32,
    pub total_frames: i32,
    pub frame_rate: f64,
}

/// Replaces every `<?...?>` block with what its script prints.
///
/// A block that fails to parse or run keeps its original text, so the author
/// can see which directive did not work instead of a silent gap.
pub(crate) fn expand_text_scripts(content: &str, clock: &TextScriptClock) -> String {
    if !content.contains("<?") {
        return content.to_owned();
    }
    let mut output = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(start) = rest.find("<?") {
        output.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("?>") else {
            output.push_str(&rest[start..]);
            return output;
        };
        let block = &after[..end];
        match evaluate_text_block(block, clock) {
            Some(value) => output.push_str(&value),
            None => output.push_str(&rest[start..start + 2 + end + 2]),
        }
        rest = &after[end + 2..];
    }
    output.push_str(rest);
    output
}

fn evaluate_text_block(block: &str, clock: &TextScriptClock) -> Option<String> {
    let block = block.trim();
    // AviUtl2 removes the `=` and wraps the rest in `mes(...)`, so both forms
    // become one statement chunk.
    let script = match block.strip_prefix('=') {
        Some(expression) => format!("mes({})", expression.trim()),
        None => block.to_owned(),
    };
    if script.len() > MAX_EXPRESSION_LENGTH {
        return None;
    }
    TEXT_SCRIPTS.with(|slot| {
        let mut slot = slot.borrow_mut();
        let vm = slot.get_or_insert_with(TextScriptVm::new);
        vm.evaluate(&script, clock)
    })
}

struct TextScriptVm {
    vm: Vm,
}

/// Runs the block in a fresh environment: `mes` collects the printed values,
/// and `setfenv(1, __env)` keeps one object's globals out of the next object's
/// script and out of the next frame. `obj.mes(text)` is the documented
/// long form of the same function.
const TEXT_SCRIPT_PRELUDE: &str = "\
local __text = {}\n\
local __emit = function(value)\n\
if value ~= nil then __text[#__text + 1] = tostring(value) end\n\
end\n\
local __env = setmetatable({ mes = __emit }, { __index = _G })\n\
setfenv(1, __env)\n\
obj.mes = __emit\n";

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
             pi = math.pi; random = math.random;",
            "=text-script-shortcuts",
        );
        Self { vm }
    }

    fn evaluate(&mut self, script: &str, clock: &TextScriptClock) -> Option<String> {
        let _ = self.vm.set_global("time", clock.time);
        let _ = self.vm.set_global("totaltime", clock.total_time);
        let _ = self.vm.set_global("frame", i64::from(clock.frame));
        let _ = self
            .vm
            .set_global("totalframe", i64::from(clock.total_frames));
        let _ = self.vm.set_global("framerate", clock.frame_rate);
        let object = self
            .vm
            .new_table()
            .with("time", clock.time)
            .with("totaltime", clock.total_time)
            .with("frame", i64::from(clock.frame))
            .with("totalframe", i64::from(clock.total_frames))
            .with("framerate", clock.frame_rate)
            .build();
        let _ = self.vm.set_global("obj", Value::Table(object));
        let mut source = String::with_capacity(TEXT_SCRIPT_PRELUDE.len() + script.len() + 64);
        source.push_str(TEXT_SCRIPT_PRELUDE);
        // Lua 5.1 cannot start a statement with `(` after a call, hence `;`.
        source.push_str(";(function() ");
        source.push_str(script);
        source.push_str(" end)()\nreturn table.concat(__text)");
        let closure = self.vm.load(source.as_bytes(), b"=text-script").ok()?;
        let values = self.vm.call_value(Value::Closure(closure), &[]).ok()?;
        // `table.concat` always yields a string; a script that printed nothing
        // yields the empty string.
        Some(match values.first()? {
            Value::Str(value) => String::from_utf8_lossy(value.as_bytes()).into_owned(),
            _ => String::new(),
        })
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
        }
    }

    #[test]
    fn text_scripts_print_through_mes_like_aviutl() {
        let clock = clock(1.5, 12.0);
        assert_eq!(expand_text_scripts("a<?=4+8?>b", &clock), "a12b");
        assert_eq!(
            expand_text_scripts("<?=string.format(\"%02d\", 5)?>", &clock),
            "05"
        );
        assert_eq!(expand_text_scripts("<? mes(obj.time) ?>", &clock), "1.5");
        assert_eq!(
            expand_text_scripts("<? t=obj.time mes(string.format(\"%.1f\", t)) ?>", &clock),
            "1.5"
        );
        assert_eq!(
            expand_text_scripts("<? mes(\"a\") mes(\"b\") ?>", &clock),
            "ab"
        );
        assert_eq!(
            expand_text_scripts("<? mes(totaltime) ?>/<? mes(totalframe) ?>", &clock),
            "12/360"
        );
        assert_eq!(
            expand_text_scripts("<? obj.mes(string.format(\"%d\", obj.frame)) ?>", &clock),
            "45"
        );
    }

    #[test]
    fn a_failed_text_script_keeps_its_literal_block() {
        let clock = clock(0.0, 1.0);
        assert_eq!(expand_text_scripts("x<?=1+?>y", &clock), "x<?=1+?>y");
        assert_eq!(
            expand_text_scripts("x<? error('boom') ?>y", &clock),
            "x<? error('boom') ?>y"
        );
        assert_eq!(expand_text_scripts("plain text", &clock), "plain text");
        assert_eq!(expand_text_scripts("dangling <?x", &clock), "dangling <?x");
    }

    #[test]
    fn each_text_script_runs_in_its_own_environment() {
        let clock = clock(0.0, 1.0);
        // `c` is assigned by the first block; the second block must not see it,
        // so `mes(c)` prints nothing.
        assert_eq!(expand_text_scripts("<? c = 7 ?><?=c?>", &clock), "");
        // Re-evaluating does not accumulate state in the shared VM.
        assert_eq!(expand_text_scripts("<?=c?>", &clock), "");
    }
}
