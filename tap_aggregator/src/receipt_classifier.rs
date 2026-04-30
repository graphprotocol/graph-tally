use anyhow::Result;

use crate::protocol_mode::ProtocolMode;

/// Validate that a batch of receipts is valid for processing
pub fn validate_receipt_batch<T>(receipts: &[T]) -> Result<ProtocolMode> {
    if receipts.is_empty() {
        return Err(anyhow::anyhow!("Cannot aggregate empty receipt batch"));
    }
    // All receipts use horizon mode
    Ok(ProtocolMode::Horizon)
}

#[cfg(test)]
mod tests {
    use tap_graph::Receipt;
    use thegraph_core::alloy::primitives::{Address, FixedBytes};

    use super::*;

    #[test]
    fn test_validate_batch() {
        let receipt = Receipt::new(
            FixedBytes::ZERO,
            Address::ZERO,
            Address::ZERO,
            Address::ZERO,
            100,
        )
        .unwrap();
        let receipts = vec![receipt];
        assert_eq!(
            validate_receipt_batch(&receipts).unwrap(),
            ProtocolMode::Horizon
        );
    }

    #[test]
    fn test_validate_empty_batch_fails() {
        let receipts: Vec<Receipt> = vec![];
        assert!(validate_receipt_batch(&receipts).is_err());
    }
}
