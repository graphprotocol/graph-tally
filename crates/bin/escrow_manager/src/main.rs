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
    // arithmetic. `u128` GRT amounts exceed f64's exact range, and these numbers decide
    // transactions.
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
        tracing::info!(
            withdraw_margin = config.withdraw_margin,
            min_withdraw_grt = config.min_withdraw_grt,
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

        let reclaim = config.withdraw_enabled.then_some(Reclaim {
            chain_now,
            margin_bps: withdraw_margin_bps,
            min_withdraw,
        });

        let mut total_target: u128 = 0;
        let mut adjustments: Vec<(Address, u128)> = Default::default();
        let mut thaws: Vec<(Address, u128)> = Default::default();
        let mut withdrawals: Vec<Address> = Default::default();
        for receiver in receivers {
            let account = escrow_accounts.get(&receiver).copied().unwrap_or_default();
            let debt = u128::max(
                debts.get(&receiver).copied().unwrap_or(0),
                config.debts.get(&receiver).copied().unwrap_or(0) as u128 * GRT,
            );
            let target = next_balance(debt, config.balance_fill_factor);
            total_target += target;

            let action = decide(&account, target, reclaim);

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
                .set(match action {
                    Action::Deposit(amount) => amount as f64 / GRT as f64,
                    _ => 0.0,
                });

            if !config.withdraw_enabled && (account.thawing > 0) {
                tracing::warn!(
                    ?receiver,
                    thawing_grt = (account.thawing as f64) / (GRT as f64),
                    "escrow is thawing but reclamation is disabled, leaving it untouched",
                );
            }

            match action {
                Action::Nothing => (),
                Action::Withdraw => {
                    tracing::info!(
                        ?receiver,
                        thawing_grt = (account.thawing as f64) / (GRT as f64),
                        "withdrawal matured",
                    );
                    withdrawals.push(receiver);
                }
                Action::Deposit(amount) => {
                    tracing::info!(
                        ?receiver,
                        balance_grt = (account.balance as f64) / (GRT as f64),
                        thawing_grt = (account.thawing as f64) / (GRT as f64),
                        debt_grt = (debt as f64) / (GRT as f64),
                        target_grt = (target as f64) / (GRT as f64),
                        adjustment_grt = (amount as f64) / (GRT as f64),
                    );
                    adjustments.push((receiver, amount));
                }
                Action::Thaw(amount) => {
                    tracing::info!(
                        ?receiver,
                        balance_grt = (account.balance as f64) / (GRT as f64),
                        debt_grt = (debt as f64) / (GRT as f64),
                        target_grt = (target as f64) / (GRT as f64),
                        thaw_grt = (amount as f64) / (GRT as f64),
                        "thawing idle escrow",
                    );
                    thaws.push((receiver, amount));
                }
            }
        }
        metrics::METRICS
            .total_target_grt
            .set(total_target as f64 / GRT as f64);

        let total_adjustment: u128 = adjustments.iter().map(|(_, a)| a).sum();
        let total_thaw: u128 = thaws.iter().map(|(_, t)| t).sum();
        // Withdrawals can only be guesstimated so we log the count
        tracing::info!(
            total_adjustment_grt = ((total_adjustment as f64) * 1e-18).ceil() as u64,
            total_thaw_grt = ((total_thaw as f64) * 1e-18).ceil() as u64,
            withdrawals = withdrawals.len(),
            "cycle plan",
        );
        metrics::METRICS
            .total_adjustment_grt
            .set(total_adjustment as f64 / GRT as f64);

        // Whenever a transaction lands, track the block number. We use this to pin the network
        // subgraph snapshot so the decision algorithm does not operate on stale data.
        let mut latest_tx_block: Option<BlockNumber> = None;

        if !thaws.is_empty() {
            if config.dry_run {
                for (receiver, tokens) in &thaws {
                    tracing::info!(
                        ?receiver,
                        tokens_grt = (*tokens as f64) / (GRT as f64),
                        "dry run: skipping thaw"
                    );
                }
            } else {
                let start = Instant::now();
                let result = contracts.thaw_many(thaws).await;
                metrics::METRICS
                    .thaw
                    .duration
                    .observe(start.elapsed().as_secs_f64());
                match result {
                    Ok(block) => {
                        metrics::METRICS.thaw.ok.inc();
                        latest_tx_block = latest_tx_block.max(Some(block));
                        tracing::info!("thaws complete");
                    }
                    Err(thaw_err) => {
                        metrics::METRICS.thaw.err.inc();
                        tracing::error!("{:#}", thaw_err.context("thaw"));
                    }
                }
            }
        }

        if !adjustments.is_empty() {
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

/// Reclamation policy for a cycle, present only when `withdraw_enabled`.
#[derive(Clone, Copy)]
struct Reclaim {
    /// Latest block timestamp, or `None` when it could not be read this cycle. Withdrawals are
    /// then skipped rather than planned against the local clock, which has no defined relationship
    /// to the block timestamp the contract compares against.
    chain_now: Option<u64>,
    margin_bps: u128,
    min_withdraw: u128,
}

/// The one action taken for a receiver in a cycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Nothing,
    /// A matured thaw is ready to come back to the payer. The contract withdraws whatever is
    /// thawing at execution time, so no amount is carried here.
    Withdraw,
    /// Effective balance is short of the target; top it up by this much.
    Deposit(u128),
    /// Idle escrow above the margin is worth reclaiming; start thawing this much.
    Thaw(u128),
}

/// Decide the single action to take for a receiver this cycle.
///
/// Exactly one action applies, which is what keeps the three transaction batches independent: no
/// receiver ever appears in more than one, so no batch depends on another having landed first.
///
/// The branches are ordered so each is decided against state the earlier ones cannot invalidate:
///
/// - Withdrawal comes first because it leaves `balance - thawing` untouched — the contract zeroes
///   both together — so taking it can never leave an account short. It only defers a deposit by a
///   cycle.
/// - Funding is decided against `balance - thawing`, matching the escrow contract's own
///   `getBalance`. Escrow already committed to leaving cannot count as coverage.
/// - A thaw only ever starts from nothing. The contract cannot grow a running thaw without
///   resetting its timer, so excess accumulating after one starts waits for the next rather than
///   being chased every cycle. Debt that grows meanwhile is covered by the deposit branch, which
///   is why the reclaimed amount need not be re-validated against it.
fn decide(account: &EscrowAccount, target: u128, reclaim: Option<Reclaim>) -> Action {
    let matured = (account.thaw_end_timestamp != 0)
        && (account.thawing > 0)
        && reclaim
            .and_then(|reclaim| reclaim.chain_now)
            .is_some_and(|now| now > account.thaw_end_timestamp);
    if matured {
        return Action::Withdraw;
    }

    let effective_balance = account.balance.saturating_sub(account.thawing);
    let deposit = target.saturating_sub(effective_balance);
    if deposit > 0 {
        return Action::Deposit(deposit);
    }

    let Some(reclaim) = reclaim else {
        return Action::Nothing;
    };
    if account.thawing > 0 {
        return Action::Nothing;
    }
    // Escrow is only reclaimed above `target * (1 + margin)`. That headroom absorbs debt growth
    // over the thawing period, and keeps ordinary fluctuation from bouncing between depositing and
    // thawing — without it the deposit and thaw branches would partition the whole range and one
    // of them would fire every cycle.
    let floor = target + (target / 10_000) * reclaim.margin_bps;
    let excess = account.balance.saturating_sub(floor);
    match (excess > 0) && (excess >= reclaim.min_withdraw) {
        true => Action::Thaw(excess),
        false => Action::Nothing,
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
    use super::{Action, EscrowAccount, Reclaim, GRT, MIN_DEPOSIT};

    const MARGIN_BPS: u128 = 2_500;
    const MIN_WITHDRAW: u128 = 500 * GRT;
    const NOW: u64 = 1_000_000;
    const MATURED: u64 = NOW - 1;
    const PENDING: u64 = NOW + 1;

    fn account(balance: u128, thawing: u128, thaw_end_timestamp: u64) -> EscrowAccount {
        EscrowAccount {
            balance,
            thawing,
            thaw_end_timestamp,
        }
    }

    fn reclaim(chain_now: Option<u64>) -> Option<Reclaim> {
        Some(Reclaim {
            chain_now,
            margin_bps: MARGIN_BPS,
            min_withdraw: MIN_WITHDRAW,
        })
    }

    fn decide(balance: u128, thawing: u128, thaw_end_timestamp: u64, target: u128) -> Action {
        super::decide(
            &account(balance, thawing, thaw_end_timestamp),
            target,
            reclaim(Some(NOW)),
        )
    }

    #[test]
    fn withdraws_a_matured_thaw() {
        // Maturity mirrors the contract's strict comparison against the block timestamp.
        assert_eq!(
            decide(20_000 * GRT, 7_500 * GRT, MATURED, 10_000 * GRT),
            Action::Withdraw
        );
        assert_eq!(
            decide(20_000 * GRT, 7_500 * GRT, NOW, 10_000 * GRT),
            Action::Nothing
        );
    }

    #[test]
    fn withdrawal_comes_before_funding() {
        // A withdrawal zeroes balance and thawing together, leaving `balance - thawing` untouched,
        // so taking it first cannot leave the account short. The deposit follows next cycle.
        assert_eq!(
            decide(20_000 * GRT, 7_500 * GRT, MATURED, 18_000 * GRT),
            Action::Withdraw
        );
    }

    #[test]
    fn funds_against_the_balance_net_of_thawing() {
        // Escrow committed to leaving cannot count as coverage, so debt growth during a thaw is
        // answered with a deposit. This is what makes re-validating the thawing amount unnecessary.
        assert_eq!(
            decide(20_000 * GRT, 7_500 * GRT, PENDING, 18_000 * GRT),
            Action::Deposit(5_500 * GRT)
        );
    }

    #[test]
    fn leaves_a_running_thaw_alone() {
        // Covered and already thawing: no attempt to chase the excess, which the contract would
        // refuse anyway without resetting the 28 day timer.
        assert_eq!(
            decide(20_000 * GRT, 7_500 * GRT, PENDING, 10_000 * GRT),
            Action::Nothing
        );
        // Even with far more idle escrow than the running thaw covers.
        assert_eq!(
            decide(100_000 * GRT, 7_500 * GRT, PENDING, 10_000 * GRT),
            Action::Nothing
        );
    }

    #[test]
    fn thaws_idle_escrow_above_the_margin() {
        let target = 10_000 * GRT;
        // Nothing is reclaimed until the balance clears target * 1.25.
        assert_eq!(decide(12_500 * GRT, 0, 0, target), Action::Nothing);
        assert_eq!(decide(12_999 * GRT, 0, 0, target), Action::Nothing);
        // Above the floor, only the excess over it is reclaimed.
        assert_eq!(
            decide(20_000 * GRT, 0, 0, target),
            Action::Thaw(7_500 * GRT)
        );
    }

    #[test]
    fn respects_the_withdrawal_minimum() {
        let target = 10_000 * GRT;
        // An excess under the minimum is not worth a 28 day round trip.
        assert_eq!(decide(12_999 * GRT, 0, 0, target), Action::Nothing);
        assert_eq!(decide(13_000 * GRT, 0, 0, target), Action::Thaw(500 * GRT));
    }

    #[test]
    fn reclamation_disabled_only_ever_deposits() {
        let idle = account(20_000 * GRT, 0, 0);
        let thawing = account(20_000 * GRT, 7_500 * GRT, MATURED);
        // No thaws started, and a matured thaw is left exactly where it is.
        assert_eq!(super::decide(&idle, 10_000 * GRT, None), Action::Nothing);
        assert_eq!(super::decide(&thawing, 10_000 * GRT, None), Action::Nothing);
        // Funding still nets out the thawing amount, matching the contract's `getBalance`.
        assert_eq!(
            super::decide(&thawing, 18_000 * GRT, None),
            Action::Deposit(5_500 * GRT)
        );
    }

    #[test]
    fn skips_withdrawals_without_a_chain_timestamp() {
        // Planning maturity against the local clock would risk reverting the whole batch, so a
        // failed timestamp read holds the withdrawal rather than guessing.
        let matured = account(20_000 * GRT, 7_500 * GRT, MATURED);
        assert_eq!(
            super::decide(&matured, 10_000 * GRT, reclaim(None)),
            Action::Nothing
        );
    }

    #[test]
    fn thawing_never_drops_the_balance_below_target() {
        // The floor sits above target by construction, so what remains after a thaw still covers
        // debt. This is what lets the thaw and deposit branches stay mutually exclusive.
        for debt_grt in [0, 1, 500, 12_345, 100_000, 580_000] {
            for balance_grt in [0, 2, 1_000, 40_000, 96_384, 1_000_000] {
                for thawing_grt in [0, 100, 20_000] {
                    let balance = balance_grt * GRT;
                    let thawing = u128::min(thawing_grt * GRT, balance);
                    let thaw_end = if thawing > 0 { PENDING } else { 0 };
                    let target = super::next_balance(debt_grt * GRT, 0.8);
                    let action = super::decide(
                        &account(balance, thawing, thaw_end),
                        target,
                        reclaim(Some(NOW)),
                    );
                    let Action::Thaw(amount) = action else {
                        continue;
                    };
                    // `thaw(0)` reverts, so a thaw is never queued for nothing.
                    assert!(amount > 0, "debt {debt_grt} balance {balance_grt}");
                    assert!(
                        balance.saturating_sub(amount) >= target,
                        "debt {debt_grt} balance {balance_grt}: \
                         thaw {amount} leaves less than target {target}",
                    );
                }
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
