//! # The Graph TAP structs
//!
//! These structs are used for communication between The Graph systems.
//!

mod v1;

#[cfg(any(test, feature = "v2"))]
pub mod v2;

pub use v1::{Receipt, ReceiptAggregateVoucher, SignedRav, SignedReceipt};
