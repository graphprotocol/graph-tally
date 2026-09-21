use std::collections::BTreeMap;

use alloy::primitives::{Address, B256};
use reqwest::Url;
use serde::Deserialize;
use serde_with::serde_as;

#[serde_as]
#[derive(Deserialize)]
pub struct Config {
    /// Authorize signers on startup.
    pub authorize_signers: bool,
    /// Skip contract calls (for testing/debugging).
    #[serde(default)]
    pub dry_run: bool,
    /// Table of minimum debts by indexer. This can be used, for example, to account for receipts
    /// missing from the kafka topic.
    pub debts: BTreeMap<Address, u64>,
    /// PaymentsEscrow contract address
    pub payments_escrow_contract: Address,
    /// GraphTallyCollector contract address
    pub graph_tally_collector_contract: Address,
    /// GRT contract for updating allowance
    pub grt_contract: Address,
    /// GRT allowance to set on startup
    pub grt_allowance: u64,
    /// Kafka configuration
    pub kafka: Kafka,
    /// Graph network subgraph URL
    #[serde_as(as = "serde_with::DisplayFromStr")]
    pub network_subgraph: Url,
    /// API key for querying subgraphs
    pub query_auth: String,
    /// RPC for executing transactions
    #[serde_as(as = "serde_with::DisplayFromStr")]
    pub rpc_url: Url,
    /// Secret key of the Graph Tally payer wallet
    pub secret_key: B256,
    /// Secret keys of the Graph Tally signer wallets, used to filter the indexer fees messages.
    pub signers: Vec<B256>,
    /// Period of the subgraph polling cycle
    pub update_interval_seconds: u32,
    /// Port for metrics server
    #[serde(default = "default_port_metrics")]
    pub port_metrics: u16,
    /// Fraction of a receiver's target escrow balance that its debt is allowed to reach before the
    /// balance is raised to the next step. This sets the funding margin: the target balance settles
    /// at roughly `debt / balance_fill_factor`, so 0.6 funds ~1.67x debt and 0.8 funds ~1.25x.
    /// Must be in the range (0, 1].
    #[serde(default = "default_balance_fill_factor")]
    pub balance_fill_factor: f64,
    /// Reclaim escrow that sits above a receiver's target balance. When disabled the manager only
    /// ever deposits, and a receiver's balance is a high-water mark of its past debt. Reclaiming
    /// takes effect via `thaw` and `withdraw`, so funds only return after the escrow contract's
    /// thawing period (28 days on mainnet) has elapsed.
    #[serde(default)]
    pub withdraw_enabled: bool,
    /// Headroom kept above the target balance before any escrow is reclaimed: escrow is only
    /// thawed above `target * (1 + withdraw_margin)`. This absorbs debt growth over the thawing
    /// period and provides hysteresis against the deposit step ladder, so it should comfortably exceed the
    /// debt growth expected for a single receiver over that period. Must be in the range [0, 1].
    #[serde(default = "default_withdraw_margin")]
    pub withdraw_margin: f64,
    /// Minimum excess, in whole GRT, required to start thawing a receiver's escrow. Excess below
    /// this is left alone. Once a receiver is already thawing, the amount tracks the excess down
    /// past this threshold rather than being cancelled, so a shrinking excess keeps its timer.
    #[serde(default = "default_min_withdraw_grt")]
    pub min_withdraw_grt: u64,
}

fn default_port_metrics() -> u16 {
    9090
}

fn default_balance_fill_factor() -> f64 {
    0.8
}

fn default_withdraw_margin() -> f64 {
    0.25
}

fn default_min_withdraw_grt() -> u64 {
    500
}

#[derive(Debug, Deserialize)]
pub struct Kafka {
    pub config: BTreeMap<String, String>,
    pub realtime_topic: String,
    pub aggregated_topic: Option<String>,
    /// Cutoff timestamp (unix milliseconds) for aggregated topic data.
    /// Aggregated records older than this are ignored.
    pub aggregated_cutoff_timestamp: Option<i64>,
}
