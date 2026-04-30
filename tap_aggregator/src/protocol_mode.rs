#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ProtocolMode {
    /// Post-horizon: V2 receipts with collection-based aggregation
    Horizon,
}
