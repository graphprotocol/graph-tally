//! Request to Tap Aggregator

use graph_tally_receipt::rav::AggregationError;
use thegraph_core::alloy::sol_types::SolStruct;

use crate::{
    receipt::{
        state::{Checked, Failed},
        ReceiptWithState,
    },
    signed_message::Eip712SignedMessage,
};

/// Request to `graph_tally_aggregator` to aggregate receipts into a Signed RAV.
#[derive(Debug)]
pub struct RavRequest<Rcpt, Rav: SolStruct> {
    /// List of checked and reserved receipts to aggregate
    pub valid_receipts: Vec<ReceiptWithState<Checked, Rcpt>>,
    /// Optional previous RAV to aggregate with
    pub previous_rav: Option<Eip712SignedMessage<Rav>>,
    /// List of failed receipt used to log invalid receipts
    pub invalid_receipts: Vec<ReceiptWithState<Failed, Rcpt>>,
    /// Expected RAV to be created
    pub expected_rav: Result<Rav, AggregationError>,
}
