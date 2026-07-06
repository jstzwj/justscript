//! `Function` global / prototype / constructor.
//!
//! TODO: spec-conformant builtins for the `function` family.

use crate::realm::Realm;

/// Install the function built-ins into `realm`.
pub fn install(_realm: &mut Realm) {
    // TODO: prototype + constructor + prototype methods.
}
