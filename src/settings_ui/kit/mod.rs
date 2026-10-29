//! Corral settings design kit. All geometry APIs accept DIPs unless explicitly named pixels.
//! The approved 2026-10-16 design is deliberately independent of any other product.
pub mod controls;
pub mod focus;
pub mod layout;
pub mod menu;
pub mod slider;
pub mod theme;

#[cfg(test)]
mod performance;

pub mod hero;
