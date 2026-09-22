use std::collections::HashMap;

use alloy::primitives::Address;
use anyhow::anyhow;
use serde_with::serde_as;
use thegraph_client_subgraphs::{Client as SubgraphClient, PaginatedQueryError};

pub async fn authorized_signers(
    network_subgraph: &mut SubgraphClient,
    payer: &Address,
) -> anyhow::Result<Vec<Address>> {
    #[derive(serde::Deserialize)]
    struct Data {
        payer: Option<Payer>,
    }
    #[derive(serde::Deserialize)]
    struct Payer {
        signers: Vec<Signer>,
    }
    #[derive(serde::Deserialize)]
    struct Signer {
        id: Address,
    }
    let data = network_subgraph
        .query::<Data>(format!(
            r#"{{ payer(id:"{payer:?}") {{ signers {{ id }} }} }}"#,
        ))
        .await
        .map_err(|err| anyhow!(err))?;
    let signers = data
        .payer
        .into_iter()
        .flat_map(|s| s.signers)
        .map(|s| s.id)
        .collect();
    Ok(signers)
}

/// Escrow account state for a single receiver.
#[derive(Clone, Copy, Debug, Default)]
pub struct EscrowAccount {
    /// Total escrow balance. Thawing does not reduce this; only withdrawing and collecting do.
    pub balance: u128,
    /// Amount currently thawing. Still collectable by the receiver, but committed to leaving.
    pub thawing: u128,
    /// Unix timestamp at which the thawing amount becomes withdrawable, or 0 if not thawing.
    pub thaw_end_timestamp: u64,
}

/// Escrow accounts held by `payer` under `collector`, keyed by receiver.
pub async fn escrow_accounts(
    network_subgraph: &mut SubgraphClient,
    payer: &Address,
    collector: &Address,
) -> anyhow::Result<HashMap<Address, EscrowAccount>> {
    let query = format!(
        r#"
        paymentsEscrowAccounts(
            block: $block
            orderBy: id
            orderDirection: asc
            first: $first
            where: {{
                id_gt: $last
                payer: "{payer:?}"
                collector: "{collector:?}"
            }}
        ) {{
            id
            balance
            totalAmountThawing
            thawEndTimestamp
            receiver {{
                id
            }}
        }}
        "#
    );
    #[serde_as]
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct EscrowAccountRow {
        #[serde_as(as = "serde_with::DisplayFromStr")]
        balance: u128,
        #[serde_as(as = "serde_with::DisplayFromStr")]
        total_amount_thawing: u128,
        #[serde_as(as = "serde_with::DisplayFromStr")]
        thaw_end_timestamp: u64,
        receiver: Receiver,
    }
    #[derive(serde::Deserialize)]
    struct Receiver {
        id: Address,
    }
    let response = network_subgraph
        .paginated_query::<EscrowAccountRow>(query, 500)
        .await;
    match response {
        Ok(accounts) => Ok(accounts
            .into_iter()
            .map(|a| {
                (
                    a.receiver.id,
                    EscrowAccount {
                        balance: a.balance,
                        thawing: a.total_amount_thawing,
                        thaw_end_timestamp: a.thaw_end_timestamp,
                    },
                )
            })
            .collect()),
        Err(PaginatedQueryError::EmptyResponse) => Ok(Default::default()),
        Err(err) => Err(anyhow!(err)),
    }
}

pub struct Allocation {
    pub id: Address,
    pub indexer: Address,
}

pub async fn active_allocations(
    network_subgraph: &mut SubgraphClient,
) -> anyhow::Result<Vec<Allocation>> {
    let query = r#"
        allocations(
            block: $block
            orderBy: id
            orderDirection: asc
            first: $first
            where: {
                id_gt: $last
                status: Active
		        isLegacy: false
            }
        ) {
            id
            indexer { id }
        }
    "#;
    #[derive(serde::Deserialize)]
    struct Allocation_ {
        id: Address,
        indexer: Indexer_,
    }
    #[derive(serde::Deserialize)]
    struct Indexer_ {
        id: Address,
    }
    Ok(network_subgraph
        .paginated_query::<Allocation_>(query, 500)
        .await
        .map_err(|err| anyhow!(err))?
        .into_iter()
        .map(|a| Allocation {
            id: a.id,
            indexer: a.indexer.id,
        })
        .collect())
}
