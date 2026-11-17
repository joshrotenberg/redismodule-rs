//! Experimental loopback HTTP verification and bounded worker ownership.
//!
//! The cdylib example supplies Redis lifecycle integration. This library owns
//! neither Redis contexts nor authentication completion tokens.

mod provider;
mod workers;

pub use provider::{Endpoint, Provider, Role, valid_credentials, valid_username};
pub use workers::{Permit, Workers};
