//! `Console` global / prototype / constructor.
//!
//! TODO: spec-conformant builtins for the `console` family.

use crate::realm::Realm;

/// Install the console built-ins into `realm`.
pub fn install(_realm: &mut Realm) {
    // TODO: prototype + constructor + prototype methods.
}
