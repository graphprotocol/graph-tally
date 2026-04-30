mod config;
mod contracts;
mod kafka;
mod metrics;
mod subgraphs;

use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write as _,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::{Duration, Instant},
};

use alloy::{primitives::Address, signers::local::PrivateKeySigner};
use anyhow::{anyhow, Context as _};
use axum::{http::StatusCode, routing, Router};
use config::Config;
use contracts::Contracts;
use prometheus::Encoder as _;
use subgraphs::{active_allocations, authorized_signers, escrow_accounts};
use thegraph_client_subgraphs::Client as SubgraphClient;
use tokio::{
    net::TcpListener,
    select,
    time::{interval, MissedTickBehavior},
};

#[global_allocator]
static ALLOC: snmalloc_rs::SnMalloc = snmalloc_rs::SnMalloc;

const GRT: u128 = 1_000_000_000_000_000_000;
const MIN_DEPOSIT: u128 = 2 * GRT;
const MAX_ADJUSTMENT: u128 = 10_000 * GRT;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let config_file = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow!("missing config file argument"))?;
    let config: Config = std::fs::read_to_string(config_file)
        .map_err(anyhow::Error::from)
        .and_then(|s| serde_json::from_str(&s).map_err(anyhow::Error::from))
        .context("failed to load config")?;

    if config.dry_run {
        tracing::info!("dry run mode enabled, contract calls will be skipped");
    }

    let payer = PrivateKeySigner::from_bytes(&config.secret_key)?;
    tracing::info!(payer = %payer.address());
    let contracts = Contracts::new(
        payer,
        config.rpc_url.clone(),
        config.grt_contract,
        config.payments_escrow_contract,
        config.graph_tally_collector_contract,
    );

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let mut network_subgraph = SubgraphClient::builder(http.clone(), config.network_subgraph)
        .with_auth_token(Some(config.query_auth.clone()))
        .build();

    let mut signers: Vec<PrivateKeySigner> = Default::default();
    for signer in config.signers {
        let signer = PrivateKeySigner::from_slice(signer.as_slice()).context("load signer key")?;
        signers.push(signer);
    }
    let signers = signers;

    if config.authorize_signers {
        let authorized_signers = authorized_signers(&mut network_subgraph, &contracts.payer())
            .await
            .context("fetch authorized signers")?;
        for signer in &signers {
            let authorized = authorized_signers.contains(&signer.address().0.into());
            tracing::info!(signer = %signer.address(), authorized);
            if authorized {
                continue;
            }
            if config.dry_run {
                tracing::info!(signer = %signer.address(), "dry run: skipping authorize_signer");
                continue;
            }
            match contracts.authorize_signer(signer).await {
                Ok(()) => tracing::info!(signer = %signer.address(), "authorized"),
                Err(err) => tracing::error!("failed to authorize signer: {err:#}"),
            };
        }
    }

    let mut allowance = contracts.allowance().await?;
    let expected_allowance = config.grt_allowance as u128 * GRT;
    tracing::info!(allowance = allowance as f64 * 1e-18);
    if allowance < expected_allowance {
        if config.dry_run {
            tracing::info!(
                expected_allowance = expected_allowance as f64 * 1e-18,
                "dry run: skipping approve"
            );
        } else {
            contracts
                .approve(expected_allowance)
                .await
                .context("approve")?;
            allowance = contracts.allowance().await?;
            tracing::info!(allowance = allowance as f64 * 1e-18);
        }
    }

    let signers: Vec<Address> = signers.into_iter().map(|s| s.address()).collect();
    let receipts = kafka::receipts(&config.kafka, signers.clone())
        .await
        .context("failed to start receipts consumer")?;
    let ravs = kafka::ravs(&config.kafka, signers)
        .await
        .context("failed to start RAVs consumer")?;

    // Host metrics on a separate server with a port that isn't open to public requests.
    let port_metrics = config.port_metrics;
    tokio::spawn(async move {
        let router = Router::new().route("/metrics", routing::get(handle_metrics));
        let metrics_listener = TcpListener::bind(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
            port_metrics,
        ))
        .await
        .expect("failed to bind metrics server");
        tracing::info!(port_metrics, "metrics server started");
        axum::serve(metrics_listener, router.into_make_service())
            .await
            .expect("metrics server failed");
    });

    let mut interval = interval(Duration::from_secs(config.update_interval_seconds as u64));
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        select! {
            _ = interval.tick() => (),
            _ = tokio::signal::ctrl_c() => anyhow::bail!("exit"),
            _ = sigterm.recv() => anyhow::bail!("exit"),
        };
        let loop_start = Instant::now();

        let allocations = match active_allocations(&mut network_subgraph).await {
            Ok(allocations) => allocations,
            Err(active_allocations_err) => {
                tracing::error!("{:#}", active_allocations_err.context("active allocations"));
                continue;
            }
        };
        let mut receivers: BTreeSet<Address> = allocations.iter().map(|a| a.indexer).collect();
        let escrow_accounts = match escrow_accounts(&mut network_subgraph, &contracts.payer()).await
        {
            Ok(escrow_accounts) => escrow_accounts,
            Err(escrow_accounts_err) => {
                if escrow_accounts_err.to_string().contains("missing block") {
                    tracing::warn!("{:#}", escrow_accounts_err.context("escrow accounts"));
                } else {
                    tracing::error!("{:#}", escrow_accounts_err.context("escrow accounts"));
                }
                continue;
            }
        };
        receivers.extend(escrow_accounts.keys());
        tracing::debug!(receivers = receivers.len());

        metrics::METRICS.receiver_count.set(receivers.len() as i64);
        metrics::METRICS
            .total_balance_grt
            .set(escrow_accounts.values().sum::<u128>() as f64 / GRT as f64);

        let mut indexer_ravs: BTreeMap<Address, u128> = Default::default();
        {
            let allocation_ravs = ravs.borrow();
            for allocation in allocations {
                if let Some(value) = allocation_ravs.get(&allocation.id) {
                    *indexer_ravs.entry(allocation.indexer).or_default() += *value;
                }
            }
        }

        let mut debts: BTreeMap<Address, u128> = Default::default();
        {
            let receipts = receipts.borrow();
            for receiver in &receivers {
                let receipts = *receipts.get(receiver).unwrap_or(&0);
                let ravs = *indexer_ravs.get(receiver).unwrap_or(&0);
                let debt = u128::max(receipts, ravs);
                debts.insert(*receiver, debt);
                tracing::info!(
                    %receiver,
                    receipts = %format!("{:.6}", receipts as f64 * 1e-18),
                    ravs = %format!("{:.6}", ravs as f64 * 1e-18),
                );
                let receiver_str = format!("{receiver:?}");
                let balance = escrow_accounts.get(receiver).copied().unwrap_or(0);
                metrics::METRICS
                    .balance_grt
                    .with_label_values(&[&receiver_str])
                    .set(balance as f64 / GRT as f64);
                metrics::METRICS
                    .debt_grt
                    .with_label_values(&[&receiver_str])
                    .set(debt as f64 / GRT as f64);
            }
        };
        metrics::METRICS
            .total_debt_grt
            .set(debts.values().sum::<u128>() as f64 / GRT as f64);

        let adjustments: Vec<(Address, u128)> = receivers
            .into_iter()
            .filter_map(|receiver| {
                let balance = escrow_accounts.get(&receiver).cloned().unwrap_or(0);
                let debt = u128::max(
                    debts.get(&receiver).copied().unwrap_or(0),
                    config.debts.get(&receiver).copied().unwrap_or(0) as u128 * GRT,
                );
                let next_balance = next_balance(debt);
                let adjustment = next_balance.saturating_sub(balance);
                if adjustment == 0 {
                    return None;
                }
                tracing::info!(
                    ?receiver,
                    balance_grt = (balance as f64) / (GRT as f64),
                    debt_grt = (debt as f64) / (GRT as f64),
                    adjustment_grt = (adjustment as f64) / (GRT as f64),
                );
                let receiver_str = format!("{receiver:?}");
                metrics::METRICS
                    .adjustment_grt
                    .with_label_values(&[&receiver_str])
                    .set(adjustment as f64 / GRT as f64);
                Some((receiver, adjustment))
            })
            .collect();

        let total_adjustment: u128 = adjustments.iter().map(|(_, a)| a).sum();
        tracing::info!(total_adjustment_grt = ((total_adjustment as f64) * 1e-18).ceil() as u64);
        metrics::METRICS
            .total_adjustment_grt
            .set(total_adjustment as f64 / GRT as f64);
        if total_adjustment > 0 {
            let adjustments = if total_adjustment <= MAX_ADJUSTMENT {
                adjustments
            } else {
                reduce_adjustments(adjustments)
            };
            if config.dry_run {
                for (receiver, adjustment) in &adjustments {
                    tracing::info!(
                        ?receiver,
                        adjustment_grt = (*adjustment as f64) / (GRT as f64),
                        "dry run: skipping deposit"
                    );
                }
                continue;
            }
            let deposit_start = Instant::now();
            let deposit_result = contracts.deposit_many(adjustments).await;
            metrics::METRICS
                .deposit
                .duration
                .observe(deposit_start.elapsed().as_secs_f64());
            let tx_block = match deposit_result {
                Ok(block) => {
                    metrics::METRICS.deposit.ok.inc();
                    block
                }
                Err(deposit_err) => {
                    metrics::METRICS.deposit.err.inc();
                    tracing::error!("{:#}", deposit_err.context("deposit"));
                    continue;
                }
            };
            network_subgraph = SubgraphClient::builder(
                network_subgraph.http_client,
                network_subgraph.subgraph_url,
            )
            .with_auth_token(Some(config.query_auth.clone()))
            .with_subgraph_latest_block(tx_block)
            .build();

            tracing::info!("adjustments complete");
        }

        metrics::METRICS
            .loop_duration
            .observe(loop_start.elapsed().as_secs_f64());
    }
}

fn next_balance(debt: u128) -> u128 {
    let mut next_round = (MIN_DEPOSIT / GRT) as u32;
    while (debt as f64) >= ((next_round as u128 * GRT) as f64 * 0.6) {
        next_round = next_round
            .saturating_mul(2)
            .min(next_round + (MAX_ADJUSTMENT / GRT) as u32);
    }
    next_round as u128 * GRT
}

fn reduce_adjustments(adjustments: Vec<(Address, u128)>) -> Vec<(Address, u128)> {
    let desired: BTreeMap<Address, u128> = adjustments.into_iter().collect();
    assert!(desired.values().sum::<u128>() > MAX_ADJUSTMENT);
    let mut adjustments: BTreeMap<Address, u128> =
        desired.keys().map(|r| (*r, MIN_DEPOSIT)).collect();
    loop {
        for (receiver, desired_value) in &desired {
            let adjustment_value = adjustments.entry(*receiver).or_default();
            if *adjustment_value < *desired_value {
                *adjustment_value = (*desired_value).min(*adjustment_value + (100 * GRT));
            }
            if adjustments.values().sum::<u128>() >= MAX_ADJUSTMENT {
                return adjustments.into_iter().collect();
            }
        }
    }
}

async fn handle_metrics() -> impl axum::response::IntoResponse {
    let encoder = prometheus::TextEncoder::new();
    let metric_families = prometheus::gather();
    let mut buffer = Vec::new();
    if let Err(metrics_encode_err) = encoder.encode(&metric_families, &mut buffer) {
        tracing::error!(%metrics_encode_err);
        buffer.clear();
        write!(&mut buffer, "Failed to encode metrics").unwrap();
        return (StatusCode::INTERNAL_SERVER_ERROR, String::new());
    }
    (StatusCode::OK, String::from_utf8(buffer).unwrap())
}

#[cfg(test)]
mod tests {
    use super::{GRT, MIN_DEPOSIT};

    #[test]
    fn next_balance() {
        let tests = [
            (0, MIN_DEPOSIT),
            (GRT, MIN_DEPOSIT),
            (MIN_DEPOSIT / 2, MIN_DEPOSIT),
            (MIN_DEPOSIT, MIN_DEPOSIT * 2),
            (MIN_DEPOSIT + 1, MIN_DEPOSIT * 2),
            (30 * GRT, 64 * GRT),
            (70 * GRT, 128 * GRT),
            (100 * GRT, 256 * GRT),
        ];
        for (debt, expected) in tests {
            assert_eq!(super::next_balance(debt), expected);
        }
    }
}
