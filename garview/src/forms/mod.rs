//! PDF form filling support
//!
//! This module provides types and state management for interactive
//! PDF form filling.

mod state;

pub use state::{FormFieldInfo, FormFieldType, FormFieldValue, FormState};
