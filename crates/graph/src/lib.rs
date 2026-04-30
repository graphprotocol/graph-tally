//! # Graph Tally structs
//!
//! These structs are used for communication between The Graph systems.
//!

mod rav;
mod receipt;

pub use rav::{ReceiptAggregateVoucher, SignedRav};
pub use receipt::{Receipt, SignedReceipt};
