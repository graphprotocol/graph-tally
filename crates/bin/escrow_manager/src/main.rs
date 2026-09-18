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

use alloy::{
    primitives::{Address, BlockNumber},
    signers::local::PrivateKeySigner,
};
use anyhow::{anyhow, Context as _};
use axum::{http::StatusCode, routing, Router};
use config::Config;
use contracts::Contracts;
use prometheus::Encoder as _;
use subgraphs::{active_allocations, authorized_signers, escrow_accounts, EscrowAccount};
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

    anyhow::ensure!(
        (config.balance_fill_factor > 0.0) && (config.balance_fill_factor <= 1.0),
        "balance_fill_factor must be in the range (0, 1], got {}",
        config.balance_fill_factor,
    );
    tracing::info!(balance_fill_factor = config.balance_fill_factor);

    anyhow::ensure!(
        config.withdraw_margin.is_finite() && (config.withdraw_margin >= 0.0),
        "withdraw_margin must be non-negative, got {}",
        config.withdraw_margin,
    );
    // Upper bound catches a fraction written as a percentage. A margin of 25 rather than 0.25 puts
    // the floor at 26x target, which no balance ever clears, so reclamation silently never runs.
    anyhow::ensure!(
        config.withdraw_margin <= 1.0,
        "withdraw_margin must be at most 1.0 (a fraction, not a percentage), got {}",
        config.withdraw_margin,
    );
    // Converted to basis points so every subsequent calculation on token amounts stays in integer
    // arithmetic. `u128` GRT amounts exceed f64's exact range, and these numbers decide transactions.
    let withdraw_margin_bps = (config.withdraw_margin * 10_000.0).round() as u128;
    let min_withdraw = config.min_withdraw_grt as u128 * GRT;

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

    if config.withdraw_enabled {
        // Read rather than assume: the period is set at deployment and capped at 90 days by the
        // escrow contract, so the schedule this manager plans against has to come from the chain.
        let thawing_period = contracts
            .withdraw_escrow_thawing_period()
            .await
            .context("get withdraw escrow thawing period")?;
        tracing::info!(
            withdraw_margin = config.withdraw_margin,
            min_withdraw_grt = config.min_withdraw_grt,
            thawing_period_days = thawing_period as f64 / 86400.0,
            "escrow reclamation enabled"
        );
    } else {
        tracing::info!("escrow reclamation disabled, deposits only");
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
            .set(escrow_accounts.values().map(|a| a.balance).sum::<u128>() as f64 / GRT as f64);
        metrics::METRICS
            .total_thawing_grt
            .set(escrow_accounts.values().map(|a| a.thawing).sum::<u128>() as f64 / GRT as f64);
        metrics::METRICS
            .thawing_count
            .set(escrow_accounts.values().filter(|a| a.thawing > 0).count() as i64);

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
                let account = escrow_accounts.get(receiver).copied().unwrap_or_default();
                metrics::METRICS
                    .balance_grt
                    .with_label_values(&[&receiver_str])
                    .set(account.balance as f64 / GRT as f64);
                metrics::METRICS
                    .thawing_grt
                    .with_label_values(&[&receiver_str])
                    .set(account.thawing as f64 / GRT as f64);
                metrics::METRICS
                    .debt_grt
                    .with_label_values(&[&receiver_str])
                    .set(debt as f64 / GRT as f64);
            }
        };
        metrics::METRICS
            .total_debt_grt
            .set(debts.values().sum::<u128>() as f64 / GRT as f64);

        // Maturity is a contract-side comparison against the block timestamp, so the reference
        // time has to come from the chain; the local clock has no defined relationship to it. A
        // failure here skips withdrawals for the cycle rather than falling back to wall clock,
        // since being early reverts the whole batch while being late costs nothing against a
        // 28 day horizon.
        let chain_now = match config.withdraw_enabled {
            false => None,
            true => match contracts.latest_block_timestamp().await {
                Ok(timestamp) => Some(timestamp),
                Err(err) => {
                    tracing::error!("{:#}", err.context("get latest block timestamp"));
                    None
                }
            },
        };

        let mut total_target: u128 = 0;
        let mut adjustments: Vec<(Address, u128)> = Default::default();
        let mut thaw_adjustments: Vec<(Address, u128)> = Default::default();
        let mut withdrawals: Vec<Address> = Default::default();
        for receiver in receivers {
            let account = escrow_accounts.get(&receiver).copied().unwrap_or_default();
            let debt = u128::max(
                debts.get(&receiver).copied().unwrap_or(0),
                config.debts.get(&receiver).copied().unwrap_or(0) as u128 * GRT,
            );
            let target = next_balance(debt, config.balance_fill_factor);
            total_target += target;

            // Amount we want thawing once this cycle's calls land. With reclamation disabled the
            // existing amount is left exactly as it is, so turning the feature off is inert.
            let desired_thaw = if config.withdraw_enabled {
                desired_thawing(&account, target, withdraw_margin_bps, min_withdraw)
            } else {
                account.thawing
            };

            // Fund against the balance that survives a pending withdrawal, so coverage still holds
            // once it lands. This is also how the escrow contract's own `getBalance` is defined.
            let effective_balance = account.balance.saturating_sub(desired_thaw);
            let adjustment = target.saturating_sub(effective_balance);

            // Record for every receiver, including those needing no action. Skipping them would
            // leave the gauges holding the last value they were set to, indefinitely.
            let receiver_str = format!("{receiver:?}");
            metrics::METRICS
                .target_grt
                .with_label_values(&[&receiver_str])
                .set(target as f64 / GRT as f64);
            metrics::METRICS
                .adjustment_grt
                .with_label_values(&[&receiver_str])
                .set(adjustment as f64 / GRT as f64);

            if desired_thaw != account.thawing {
                tracing::info!(
                    ?receiver,
                    balance_grt = (account.balance as f64) / (GRT as f64),
                    debt_grt = (debt as f64) / (GRT as f64),
                    target_grt = (target as f64) / (GRT as f64),
                    thawing_grt = (account.thawing as f64) / (GRT as f64),
                    desired_thawing_grt = (desired_thaw as f64) / (GRT as f64),
                    "thaw adjustment",
                );
                thaw_adjustments.push((receiver, desired_thaw));
            } else if !config.withdraw_enabled && (account.thawing > 0) {
                tracing::warn!(
                    ?receiver,
                    thawing_grt = (account.thawing as f64) / (GRT as f64),
                    "escrow is thawing but reclamation is disabled, leaving it untouched",
                );
            }

            // Only withdraw a thaw that has matured and still has a justified amount. Cancelling
            // clears the timestamp, so a receiver being cancelled this cycle must be left out or
            // the withdrawal reverts and takes the whole batch with it. The comparison mirrors the
            // contract's, which is strict.
            let matured = (account.thaw_end_timestamp != 0)
                && chain_now.is_some_and(|now| now > account.thaw_end_timestamp);
            if config.withdraw_enabled && matured && (desired_thaw > 0) {
                tracing::info!(
                    ?receiver,
                    tokens_grt = (desired_thaw as f64) / (GRT as f64),
                    "withdrawal matured",
                );
                withdrawals.push(receiver);
            }

            if adjustment == 0 {
                continue;
            }
            tracing::info!(
                ?receiver,
                balance_grt = (account.balance as f64) / (GRT as f64),
                effective_balance_grt = (effective_balance as f64) / (GRT as f64),
                debt_grt = (debt as f64) / (GRT as f64),
                target_grt = (target as f64) / (GRT as f64),
                adjustment_grt = (adjustment as f64) / (GRT as f64),
            );
            adjustments.push((receiver, adjustment));
        }
        metrics::METRICS
            .total_target_grt
            .set(total_target as f64 / GRT as f64);

        let total_adjustment: u128 = adjustments.iter().map(|(_, a)| a).sum();
        tracing::info!(total_adjustment_grt = ((total_adjustment as f64) * 1e-18).ceil() as u64);
        metrics::METRICS
            .total_adjustment_grt
            .set(total_adjustment as f64 / GRT as f64);
        // Tracked across all three phases so a failure in one does not skip the others, and the
        // subgraph client is only pinned forward once.
        let mut latest_tx_block: Option<BlockNumber> = None;

        // Thaw adjustments run first: shrinking or cancelling a thaw is the protective action, and
        // the deposit amounts computed above already assume it has happened.
        if !thaw_adjustments.is_empty() {
            if config.dry_run {
                for (receiver, tokens) in &thaw_adjustments {
                    tracing::info!(
                        ?receiver,
                        tokens_grt = (*tokens as f64) / (GRT as f64),
                        "dry run: skipping adjust thaw"
                    );
                }
            } else {
                let start = Instant::now();
                let result = contracts.adjust_thaw_many(thaw_adjustments).await;
                metrics::METRICS
                    .adjust_thaw
                    .duration
                    .observe(start.elapsed().as_secs_f64());
                match result {
                    Ok(block) => {
                        metrics::METRICS.adjust_thaw.ok.inc();
                        latest_tx_block = latest_tx_block.max(Some(block));
                        tracing::info!("thaw adjustments complete");
                    }
                    Err(adjust_thaw_err) => {
                        metrics::METRICS.adjust_thaw.err.inc();
                        tracing::error!("{:#}", adjust_thaw_err.context("adjust thaw"));
                    }
                }
            }
        }

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
            } else {
                let deposit_start = Instant::now();
                let deposit_result = contracts.deposit_many(adjustments).await;
                metrics::METRICS
                    .deposit
                    .duration
                    .observe(deposit_start.elapsed().as_secs_f64());
                match deposit_result {
                    Ok(block) => {
                        metrics::METRICS.deposit.ok.inc();
                        latest_tx_block = latest_tx_block.max(Some(block));
                        tracing::info!("adjustments complete");
                    }
                    Err(deposit_err) => {
                        metrics::METRICS.deposit.err.inc();
                        tracing::error!("{:#}", deposit_err.context("deposit"));
                    }
                }
            }
        }

        if !withdrawals.is_empty() {
            if config.dry_run {
                for receiver in &withdrawals {
                    tracing::info!(?receiver, "dry run: skipping withdraw");
                }
            } else {
                let start = Instant::now();
                let result = contracts.withdraw_many(withdrawals).await;
                metrics::METRICS
                    .withdraw
                    .duration
                    .observe(start.elapsed().as_secs_f64());
                match result {
                    Ok(block) => {
                        metrics::METRICS.withdraw.ok.inc();
                        latest_tx_block = latest_tx_block.max(Some(block));
                        tracing::info!("withdrawals complete");
                    }
                    Err(withdraw_err) => {
                        metrics::METRICS.withdraw.err.inc();
                        tracing::error!("{:#}", withdraw_err.context("withdraw"));
                    }
                }
            }
        }

        if let Some(tx_block) = latest_tx_block {
            network_subgraph = SubgraphClient::builder(
                network_subgraph.http_client,
                network_subgraph.subgraph_url,
            )
            .with_auth_token(Some(config.query_auth.clone()))
            .with_subgraph_latest_block(tx_block)
            .build();
        }

        metrics::METRICS
            .loop_duration
            .observe(loop_start.elapsed().as_secs_f64());
    }
}

/// Target escrow balance for a receiver with the given debt. The balance steps up while debt
/// reaches `fill_factor` of the current step, so the target settles at roughly `debt / fill_factor`
/// once the steps are fine-grained (above `MAX_ADJUSTMENT`, where they stop doubling).
fn next_balance(debt: u128, fill_factor: f64) -> u128 {
    let mut next_round = (MIN_DEPOSIT / GRT) as u32;
    while (debt as f64) >= ((next_round as u128 * GRT) as f64 * fill_factor) {
        next_round = next_round
            .saturating_mul(2)
            .min(next_round + (MAX_ADJUSTMENT / GRT) as u32);
    }
    next_round as u128 * GRT
}

/// Escrow that should be thawing for a receiver once this cycle's calls land.
///
/// Escrow is only reclaimed above `target * (1 + margin)`. That headroom absorbs debt growth over
/// the thawing period, and provides hysteresis against the target's step ladder.
///
/// `adjustThaw` refuses an increase that would reset a running timer, so a thaw already in flight
/// can only be tracked downward. This mirrors that, both so the amount withdrawn at maturity has
/// been continuously re-validated against current debt, and so funding decisions agree with what
/// the contract will actually hold. The minimum only gates starting a thaw: once one is running the
/// amount follows the excess down past the minimum, because shrinking keeps the maturity timestamp
/// whereas cancelling would forfeit the whole thawing period.
fn desired_thawing(
    account: &EscrowAccount,
    target: u128,
    margin_bps: u128,
    min_withdraw: u128,
) -> u128 {
    let floor = target + (target / 10_000) * margin_bps;
    let excess = account.balance.saturating_sub(floor);
    if account.thawing > 0 {
        excess.min(account.thawing)
    } else if excess >= min_withdraw {
        excess
    } else {
        0
    }
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
    use super::{EscrowAccount, GRT, MIN_DEPOSIT};

    const MARGIN_BPS: u128 = 2_500;
    const MIN_WITHDRAW: u128 = 500 * GRT;

    fn account(balance: u128, thawing: u128) -> EscrowAccount {
        EscrowAccount {
            balance,
            thawing,
            thaw_end_timestamp: if thawing > 0 { 1 } else { 0 },
        }
    }

    fn desired(balance: u128, thawing: u128, target: u128) -> u128 {
        super::desired_thawing(&account(balance, thawing), target, MARGIN_BPS, MIN_WITHDRAW)
    }

    #[test]
    fn desired_thawing_applies_margin_above_target() {
        let target = 10_000 * GRT;
        // Nothing is reclaimed until the balance clears target * 1.25.
        assert_eq!(desired(12_500 * GRT, 0, target), 0);
        assert_eq!(desired(12_999 * GRT, 0, target), 0);
        // Above the floor, only the excess over it is reclaimed.
        assert_eq!(desired(20_000 * GRT, 0, target), 7_500 * GRT);
    }

    #[test]
    fn desired_thawing_respects_minimum_when_starting() {
        let target = 10_000 * GRT;
        // An excess under the minimum is not worth a 28 day round trip.
        assert_eq!(desired(12_999 * GRT, 0, target), 0);
        assert_eq!(desired(13_000 * GRT, 0, target), 500 * GRT);
    }

    #[test]
    fn desired_thawing_tracks_a_running_thaw_downward() {
        // Target climbed a rung, so the justified excess shrank. Following it down preserves the
        // maturity timestamp; cancelling and re-thawing would forfeit the thawing period.
        assert_eq!(
            desired(20_000 * GRT, 7_500 * GRT, 14_000 * GRT),
            2_500 * GRT
        );
        // Below the minimum too: the threshold only gates starting a thaw.
        assert_eq!(desired(20_000 * GRT, 7_500 * GRT, 15_900 * GRT), 125 * GRT);
        // Excess gone entirely: cancel.
        assert_eq!(desired(20_000 * GRT, 7_500 * GRT, 16_000 * GRT), 0);
        assert_eq!(desired(20_000 * GRT, 7_500 * GRT, 30_000 * GRT), 0);
    }

    #[test]
    fn desired_thawing_never_grows_a_running_thaw() {
        // The contract refuses an increase that would reset the timer, so asking for one would
        // silently do nothing. Report what the contract will actually hold instead.
        let target = 10_000 * GRT;
        assert_eq!(desired(40_000 * GRT, 7_500 * GRT, target), 7_500 * GRT);
    }

    #[test]
    fn deposits_and_reclaims_are_mutually_exclusive() {
        // A receiver can never be funded and reclaimed in the same cycle: reclaiming leaves the
        // effective balance at the floor, which is above target by construction.
        for debt_grt in [0, 1, 500, 12_345, 100_000, 580_000] {
            for balance_grt in [0, 2, 1_000, 40_000, 96_384, 1_000_000] {
                for thawing_grt in [0, 100, 20_000] {
                    let balance = balance_grt * GRT;
                    let thawing = u128::min(thawing_grt * GRT, balance);
                    let target = super::next_balance(debt_grt * GRT, 0.8);
                    let thaw = super::desired_thawing(
                        &account(balance, thawing),
                        target,
                        MARGIN_BPS,
                        MIN_WITHDRAW,
                    );
                    let adjustment = target.saturating_sub(balance.saturating_sub(thaw));
                    assert!(
                        (thaw == 0) || (adjustment == 0),
                        "debt {debt_grt} balance {balance_grt} thawing {thawing_grt}: \
                         thaw {thaw} and adjustment {adjustment}",
                    );
                }
            }
        }
    }

    #[test]
    fn desired_thawing_never_exceeds_balance() {
        for balance_grt in [0, 2, 1_000, 96_384] {
            for target_grt in [2, 1_000, 96_384] {
                let balance = balance_grt * GRT;
                let thaw = super::desired_thawing(
                    &account(balance, 0),
                    target_grt * GRT,
                    MARGIN_BPS,
                    MIN_WITHDRAW,
                );
                assert!(thaw <= balance, "balance {balance_grt} target {target_grt}");
            }
        }
    }

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
            assert_eq!(super::next_balance(debt, 0.6), expected);
        }
    }

    #[test]
    fn next_balance_fill_factor() {
        // A higher fill factor packs debt closer to the target balance, funding a thinner margin.
        let tests = [
            (0, MIN_DEPOSIT),
            (MIN_DEPOSIT, MIN_DEPOSIT * 2),
            (30 * GRT, 64 * GRT),
            (70 * GRT, 128 * GRT),
            // 100 GRT of debt is funded to 256 GRT at 0.6, but only 128 GRT at 0.8.
            (100 * GRT, 128 * GRT),
        ];
        for (debt, expected) in tests {
            assert_eq!(super::next_balance(debt, 0.8), expected);
        }
    }

    #[test]
    fn next_balance_margin_converges_above_step_cap() {
        // Once the steps stop doubling, the target tracks debt / fill_factor closely.
        for fill_factor in [0.6, 0.8, 0.95] {
            let debt = 580_000 * GRT;
            let target = super::next_balance(debt, fill_factor);
            let ratio = (target as f64) / (debt as f64);
            let expected = 1.0 / fill_factor;
            assert!(
                (ratio >= expected) && (ratio < (expected + 0.05)),
                "fill_factor {fill_factor}: ratio {ratio} not just above {expected}",
            );
        }
    }
}
