//! `Object` global, constructor and `%ObjectPrototype%`.
//!
//! TODO: `Object.create`, `Object.keys`, `Object.prototype.hasOwnProperty`, etc.

use crate::realm::Realm;

/// Install the Object built-ins into `realm`.
pub fn install(_realm: &mut Realm) {
    // TODO: create %ObjectPrototype% with hasOwnProperty / toString / valueOf,
    // then the `Object` constructor function and the `Object.prototype` link.
}
