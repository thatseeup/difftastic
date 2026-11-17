//! Library target for embedding difftastic. Only [`api`] is public;
//! the other modules are shared with the `difft` binary unchanged.

// Cargo sets CARGO_BIN_NAME only when compiling bin targets, but
// `options` (shared with `difft`) reads it at compile time.
macro_rules! env {
    ("CARGO_BIN_NAME") => {
        "difft"
    };
    ($name:literal) => {
        ::core::env!($name)
    };
}

// Code only the `difft` binary reaches is dead from the library's view.
#[allow(dead_code)]
mod conflicts;
#[allow(dead_code)]
mod constants;
#[allow(dead_code)]
mod diff;
#[allow(dead_code)]
mod display;
#[allow(dead_code)]
mod exit_codes;
#[allow(dead_code)]
mod files;
#[allow(dead_code)]
mod gitattributes;
#[allow(dead_code)]
mod hash;
#[allow(dead_code)]
mod line_parser;
#[allow(dead_code)]
mod lines;
#[allow(dead_code)]
mod options;
#[allow(dead_code)]
mod parse;
#[allow(dead_code)]
mod summary;
#[allow(dead_code)]
mod version;
#[allow(dead_code)]
mod words;

pub mod api;

// Unit tests inside the shared modules import `crate::syntax`, as `main.rs` provides.
#[cfg(test)]
use parse::syntax;

#[macro_use]
extern crate log;
