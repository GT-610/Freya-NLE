//! Evaluates AviUtl-style `=expression` parameter values.
//!
//! A parameter whose stored value is a string beginning with `=` is a Lua
//! expression instead of a literal. It is evaluated with `time` (seconds from
//! the start of the object) and `value` (the keyframe-resolved value for the
//! same parameter) in scope, matching what the Qt frontend's expression host
//! exposed. The evaluator is lazy: a project without expressions never pays for
//! a Lua state.

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
             pi = math.pi; random = math.random;",
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
}
