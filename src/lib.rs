pub mod desktop;
pub mod error;
pub mod mcp;
pub mod platform;
pub mod runtime;
pub mod session;
pub mod types;

#[cfg(test)]
extern crate self as agent_desktop;
#[cfg(test)]
#[path = "../tests/support/fakes.rs"]
mod test_support;
