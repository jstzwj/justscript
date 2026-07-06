//! `Symbol` global / prototype / constructor.
//!
//! TODO: spec-conformant builtins for the `symbol` family.

use crate::realm::Realm;

/// Install the symbol built-ins into `realm`.
pub fn install(_realm: &mut Realm) {
    // TODO: prototype + constructor + prototype methods.
}
