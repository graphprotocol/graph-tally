#![doc = include_str!("../README.md")]

use std::{sync::Arc, time::Duration};

use anyhow::{anyhow, Context as _, Result};
use clap::Parser;
use graph_tally_aggregator::{metrics, server, signers, signers::SignerRegistry};
use graph_tally_core::graph_tally_eip712_domain;
use log::{debug, info};
use thegraph_core::alloy::{
    dyn_abi::Eip712Domain, primitives::Address, signers::local::PrivateKeySigner,
};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Port to listen on for JSON-RPC requests.
    /// Defaults to 8080.
    #[arg(long, default_value_t = 8080, env = "GRAPH_TALLY_PORT")]
    port: u16,

    /// Signing key per payer, as `;`-separated `<payer address>=<signer private key>` entries.
    ///
    /// The key is a signer authorized on chain for that payer, not the payer's own key. A RAV
    /// carries the payer named in the receipts it aggregates, and the collector requires the
    /// RAV's signer to be authorized for that payer -- so each payer served needs its own key.
    #[arg(long, env = "GRAPH_TALLY_SIGNERS")]
    signers: Option<String>,

    /// Additional accepted receipt signers per payer, as `;`-separated
    /// `<payer address>=<signer address>` entries. Repeat a payer to list several.
    ///
    /// Each payer's own signing key is already accepted and need not be listed. Every address
    /// listed here must be authorized for the payer it is listed under -- a signer accepted
    /// for one payer does not vouch for another's receipts.
    #[arg(long, env = "GRAPH_TALLY_ACCEPTED_SIGNERS")]
    accepted_signers: Option<String>,

    /// Maximum request body size in bytes.
    /// Defaults to 10MB.
    #[arg(long, default_value_t = 10 * 1024 * 1024, env = "GRAPH_TALLY_MAX_REQUEST_BODY_SIZE")]
    max_request_body_size: u32,

    /// Maximum response body size in bytes.
    /// Defaults to 100kB.
    #[arg(long, default_value_t = 100 * 1024, env = "GRAPH_TALLY_MAX_RESPONSE_BODY_SIZE")]
    max_response_body_size: u32,

    /// Maximum number of concurrent connections.
    /// Defaults to 32.
    #[arg(long, default_value_t = 32, env = "GRAPH_TALLY_MAX_CONNECTIONS")]
    max_connections: u32,

    /// Maximum time in seconds allowed for processing a request.
    /// This timeout protects against Slowloris-style DoS attacks by ensuring
    /// that connections cannot be held open indefinitely.
    /// Defaults to 60 seconds.
    #[arg(long, default_value_t = 60, env = "GRAPH_TALLY_REQUEST_TIMEOUT_SECS")]
    request_timeout_secs: u64,

    /// Metrics server port.
    /// Defaults to 5000.
    #[arg(long, default_value_t = 5000, env = "GRAPH_TALLY_METRICS_PORT")]
    metrics_port: u16,

    /// Domain chain ID to be used for the EIP-712 domain separator.
    #[arg(long, env = "GRAPH_TALLY_DOMAIN_CHAIN_ID")]
    domain_chain_id: Option<String>,

    /// Domain verifying contract to be used for the EIP-712 domain separator.
    #[arg(long, env = "GRAPH_TALLY_DOMAIN_VERIFYING_CONTRACT")]
    domain_verifying_contract: Option<Address>,

    #[arg(long, env = "GRAPH_TALLY_KAFKA_CONFIG")]
    kafka_config: Option<String>,
}

impl std::fmt::Debug for Args {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Args")
            .field("port", &self.port)
            .field("signers", &"[REDACTED]")
            .field("accepted_signers", &self.accepted_signers)
            .field("max_request_body_size", &self.max_request_body_size)
            .field("max_response_body_size", &self.max_response_body_size)
            .field("max_connections", &self.max_connections)
            .field("request_timeout_secs", &self.request_timeout_secs)
            .field("metrics_port", &self.metrics_port)
            .field("domain_chain_id", &self.domain_chain_id)
            .field("domain_verifying_contract", &self.domain_verifying_contract)
            .field("kafka_config", &self.kafka_config)
            .finish()
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize the logger.
    // Set the log level by setting the RUST_LOG environment variable.
    // We prefer using tracing_subscriber as the logging backend because jsonrpsee
    // uses it, and it shows jsonrpsee log spans in the logs (to see client IP, etc).
    // See https://github.com/paritytech/jsonrpsee/pull/922 for more info.
    tracing_subscriber::fmt::init();

    let args = Args::parse();
    debug!("Settings: {args:?}");

    // Start the metrics server.
    // We just let it gracelessly get killed at the end of main()
    tokio::spawn(metrics::run_server(args.metrics_port));

    let signers = Arc::new(signer_registry(
        // The payer is the piece an older single-key config never recorded, so say where to
        // find it rather than leaving the reader to work out which payer their key serves.
        args.signers.as_deref().context(
            "GRAPH_TALLY_SIGNERS is required: `;`-separated `<payer address>=<signer private \
             key>` entries. The payer for an existing signing key is the `authorizer` returned \
             by GraphTallyCollector.authorizations(<that key's address>).",
        )?,
        args.accepted_signers.as_deref(),
    )?);
    // Logged per payer because a signer bound to the wrong payer produces RAVs that are
    // rejected downstream with nothing in this process's logs to say why.
    for (payer, signer, accepted) in signers.summary() {
        info!("payer {payer:#40x} signs with {signer:#40x}");
        info!("  accepted receipt signers: {accepted:?}");
    }

    // Create the EIP-712 domain separator.
    let domain_separator = create_eip712_domain(&args)?;

    let kafka = match args.kafka_config {
        None => None,
        Some(config) => {
            let mut client = rdkafka::ClientConfig::new();
            for (key, value) in config.split(';').filter_map(|s| s.split_once('=')) {
                client.set(key, value);
            }
            Some(client.create()?)
        }
    };

    // Start the JSON-RPC server.
    // This await is non-blocking
    let (handle, _) = server::run_server(
        args.port,
        signers,
        domain_separator,
        args.max_request_body_size,
        args.max_response_body_size,
        args.max_connections,
        Duration::from_secs(args.request_timeout_secs),
        kafka,
    )
    .await?;
    info!("Server started. Listening on port {}.", args.port);

    let _ = handle.await;

    // If we're here, we've received a signal to exit.
    info!("Shutting down...");
    Ok(())
}

/// Split `;`-separated `key=value` pairs, as `--kafka-config` does.
///
/// A segment with no `=` is an error rather than a skip. Dropping one would leave a payer
/// unserved or a signer unaccepted, and neither shows up until a request arrives and is
/// refused -- by which point the failure is the indexer's, not the operator's.
///
/// Empty segments are still ignored, so a trailing or doubled `;` stays valid.
fn pairs(raw: &str) -> Result<Vec<(&str, &str)>> {
    raw.split(';')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            entry
                .split_once('=')
                .map(|(key, value)| (key.trim(), value.trim()))
                .ok_or_else(|| anyhow!("malformed entry {entry:?}, expected `key=value`"))
        })
        .collect()
}

/// Parse `--signers` / `--accepted-signers` into a [`SignerRegistry`].
fn signer_registry(signers: &str, accepted_signers: Option<&str>) -> Result<SignerRegistry> {
    let signing_keys = pairs(signers)
        .context("GRAPH_TALLY_SIGNERS")?
        .into_iter()
        .map(|(payer, key)| {
            let payer: Address = payer
                .parse()
                .with_context(|| format!("GRAPH_TALLY_SIGNERS: parse payer address {payer:?}"))?;
            let key: PrivateKeySigner = key
                .parse()
                .with_context(|| format!("GRAPH_TALLY_SIGNERS: parse signing key for {payer}"))?;
            Ok((payer, key))
        })
        .collect::<Result<Vec<_>>>()?;

    let accepted = accepted_signers
        .map(pairs)
        .transpose()
        .context("GRAPH_TALLY_ACCEPTED_SIGNERS")?
        .unwrap_or_default()
        .into_iter()
        .map(|(payer, signer)| {
            let payer: Address = payer.parse().with_context(|| {
                format!("GRAPH_TALLY_ACCEPTED_SIGNERS: parse payer address {payer:?}")
            })?;
            let signer: Address = signer.parse().with_context(|| {
                format!("GRAPH_TALLY_ACCEPTED_SIGNERS: parse signer address for {payer}")
            })?;
            Ok((payer, signer))
        })
        .collect::<Result<Vec<_>>>()?;

    signers::build(signing_keys, accepted).context("GRAPH_TALLY_SIGNERS")
}

/// Creates the Graph Tally EIP-712 domain separator based on the provided arguments
fn create_eip712_domain(args: &Args) -> Result<Eip712Domain> {
    if args.domain_chain_id.is_some() {
        debug!("Parsing domain chain ID...");
    }
    let chain_id: Option<u64> = args
        .domain_chain_id
        .as_ref()
        .map(|s| s.parse())
        .transpose()?;

    let verifying_contract = args.domain_verifying_contract;

    // Create the EIP-712 domain separator.
    Ok(graph_tally_eip712_domain(
        chain_id.unwrap_or(1),
        verifying_contract.unwrap_or_default(),
    ))
}

#[cfg(test)]
mod tests {
    use super::{pairs, signer_registry};

    const PAYER_A: &str = "0x1111111111111111111111111111111111111111";
    const PAYER_B: &str = "0x2222222222222222222222222222222222222222";
    const KEY_A: &str = "0x0000000000000000000000000000000000000000000000000000000000000001";
    const KEY_B: &str = "0x0000000000000000000000000000000000000000000000000000000000000002";
    const SIGNER: &str = "0x3333333333333333333333333333333333333333";

    #[test]
    fn splits_pairs_and_trims() {
        assert_eq!(pairs("a=1;b=2").unwrap(), vec![("a", "1"), ("b", "2")]);
        assert_eq!(
            pairs("  a = 1 ; b = 2 ").unwrap(),
            vec![("a", "1"), ("b", "2")]
        );
        // Only the first `=` separates, so a value may contain one.
        assert_eq!(pairs("a=1=2").unwrap(), vec![("a", "1=2")]);
    }

    #[test]
    fn empty_segments_are_ignored() {
        // Trailing and doubled separators stay valid.
        assert_eq!(pairs("a=1;").unwrap(), vec![("a", "1")]);
        assert_eq!(pairs("a=1;;b=2").unwrap(), vec![("a", "1"), ("b", "2")]);
        assert_eq!(pairs(" ; ").unwrap(), vec![]);
    }

    #[test]
    fn a_segment_without_an_equals_is_an_error() {
        // Skipping it would drop one payer from a multi-payer config and only surface when
        // that payer sent a request.
        let err = pairs("a=1;oops;b=2").unwrap_err().to_string();
        assert!(err.contains("oops"), "{err}");
        assert!(pairs("oops").is_err());
    }

    #[test]
    fn a_typo_in_one_signers_entry_fails_startup() {
        let good = format!("{PAYER_A}={KEY_A};{PAYER_B}={KEY_B}");
        assert!(signer_registry(&good, None).is_ok());

        // Same config with the second `=` fat-fingered into a space.
        let typo = format!("{PAYER_A}={KEY_A};{PAYER_B} {KEY_B}");
        // `.err()` rather than `unwrap_err()`: the Ok type holds private keys and is
        // deliberately not Debug.
        let err = signer_registry(&typo, None)
            .err()
            .expect("malformed entry should fail");
        assert!(
            format!("{err:#}").contains("GRAPH_TALLY_SIGNERS"),
            "{err:#}"
        );
    }

    #[test]
    fn a_typo_in_accepted_signers_fails_startup() {
        let signers = format!("{PAYER_A}={KEY_A}");
        assert!(signer_registry(&signers, Some(&format!("{PAYER_A}={SIGNER}"))).is_ok());

        let err = signer_registry(&signers, Some(&format!("{PAYER_A} {SIGNER}")))
            .err()
            .expect("malformed entry should fail");
        assert!(
            format!("{err:#}").contains("GRAPH_TALLY_ACCEPTED_SIGNERS"),
            "{err:#}"
        );
    }
}
