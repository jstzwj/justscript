//! The constant pool of a [`crate::BytecodeModule`]: deduplicated literals
//! (numbers, strings) referenced by `LdaConst`/`GetGlobal`/etc.

use js_runtime::value::{JsBigInt, JsString, Value};
use std::collections::HashMap;

/// A deduplicated table of compile-time constants, addressed by 16-bit index.
#[derive(Debug, Default)]
pub struct ConstantPool {
    items: Vec<Value>,
    /// String → index, to dedupe string constants.
    by_str: HashMap<String, u16>,
    /// Number bits → index.
    by_num: HashMap<u64, u16>,
    /// Canonical decimal BigInt value -> index.
    by_bigint: HashMap<String, u16>,
}

impl ConstantPool {
    pub fn new() -> ConstantPool {
        ConstantPool::default()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Intern a string constant; returns its index.
    pub fn intern_str(&mut self, s: impl Into<String>) -> u16 {
        let s = s.into();
        if let Some(&i) = self.by_str.get(&s) {
            return i;
        }
        let idx = self.push(Value::string(JsString::new(s.clone())));
        self.by_str.insert(s, idx);
        idx
    }

    /// Intern a number constant.
    pub fn intern_num(&mut self, n: f64) -> u16 {
        let key = n.to_bits();
        if let Some(&i) = self.by_num.get(&key) {
            return i;
        }
        let idx = self.push(Value::number(n));
        self.by_num.insert(key, idx);
        idx
    }

    /// Intern a small integer constant (kept as `Integer` so the VM uses the
    /// integer fast path). Callers should only use this for integral values
    /// that comfortably fit in an `i32`.
    pub fn intern_int(&mut self, i: i32) -> u16 {
        // Dedupe against the f64 bit pattern of the same value so that `1` and
        // `1.0` (if both appear) collapse to one entry.
        let key = (i as f64).to_bits();
        if let Some(&idx) = self.by_num.get(&key) {
            return idx;
        }
        let idx = self.push(Value::integer(i));
        self.by_num.insert(key, idx);
        idx
    }

    pub fn intern_bigint(&mut self, raw: &str) -> u16 {
        let bigint = JsBigInt::from_literal(raw);
        if let Some(&index) = self.by_bigint.get(&bigint.0) {
            return index;
        }
        let key = bigint.0.clone();
        let index = self.push(Value::bigint(bigint));
        self.by_bigint.insert(key, index);
        index
    }

    fn push(&mut self, v: Value) -> u16 {
        let idx = self.items.len() as u16;
        self.items.push(v);
        idx
    }

    /// Fetch a constant by index (panics on out-of-bounds in debug).
    pub fn get(&self, idx: u16) -> &Value {
        &self.items[idx as usize]
    }

    /// Read-only access to all constants.
    pub fn items(&self) -> &[Value] {
        &self.items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interns_and_dedupes() {
        let mut pool = ConstantPool::new();
        let a = pool.intern_str("hi");
        let b = pool.intern_str("hi");
        let c = pool.intern_num(3.0);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn bigint_literals_are_exact_and_deduped_across_radices() {
        let mut pool = ConstantPool::new();
        let decimal = pool.intern_bigint("18446744073709551616n");
        let hexadecimal = pool.intern_bigint("0x1_0000_0000_0000_0000n");
        assert_eq!(decimal, hexadecimal);
        assert!(matches!(
            pool.get(decimal).data(),
            js_runtime::value::ValueData::BigInt(value)
                if value.0 == "18446744073709551616"
        ));
    }
}
