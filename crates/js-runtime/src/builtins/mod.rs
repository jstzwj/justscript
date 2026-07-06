//! Built-in objects, prototypes and constructors.
//!
//! Each submodule registers one built-in family into a [`crate::realm::Realm`]
//! during realm initialization. The skeleton defines the registration hooks
//! and stub implementations; full spec conformance is filled in incrementally.

pub mod array;
pub mod bigint;
pub mod boolean;
pub mod console;
pub mod function;
pub mod number;
pub mod object;
pub mod string;
pub mod symbol;

use crate::realm::Realm;

/// Install all built-ins into the realm. Called once per realm at creation.
pub fn install_all(realm: &mut Realm) {
    object::install(realm);
    function::install(realm);
    array::install(realm);
    string::install(realm);
    number::install(realm);
    boolean::install(realm);
    symbol::install(realm);
    bigint::install(realm);
    console::install(realm);
}
