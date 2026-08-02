//! Per-function local-variable slot allocation.

use std::collections::HashMap;

/// Maps source-level names to local slot indices within one function.
#[derive(Debug, Default)]
pub struct LocalTable {
    by_name: HashMap<String, u16>,
    count: u16,
    /// Number of slots reserved for parameters at the front of the frame.
    pub param_count: u16,
}

impl LocalTable {
    pub fn new(param_count: u16) -> LocalTable {
        LocalTable {
            param_count,
            count: param_count,
            ..Default::default()
        }
    }

    /// Allocate (or reuse) a slot for `name`. Returns its index.
    pub fn intern(&mut self, name: impl Into<String>) -> u16 {
        let name = name.into();
        if let Some(&i) = self.by_name.get(&name) {
            return i;
        }
        let i = self.count;
        self.count += 1;
        self.by_name.insert(name, i);
        i
    }

    /// Look up an existing slot.
    pub fn get(&self, name: &str) -> Option<u16> {
        self.by_name.get(name).copied()
    }

    pub fn slot_count(&self) -> u16 {
        self.count
    }

    /// Source binding names and their stable frame slots.
    pub fn entries(&self) -> impl Iterator<Item = (&str, u16)> {
        self.by_name
            .iter()
            .map(|(name, slot)| (name.as_str(), *slot))
    }
}
