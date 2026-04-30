# Graph Tally

Graph Tally (formerly TAP - Timeline Aggregation Protocol) is a trust-minimized payment system between Gateways and Indexers in [The Graph](https://thegraph.com) network. It supports arbitrary data services built on Graph Horizon.

## How it works

1. **Gateways** send signed Receipts to Indexers alongside each query
2. **Indexers** collect Receipts and periodically request aggregation
3. **Aggregator** bundles Receipts into a signed Receipt Aggregate Voucher (RAV)
4. **Indexers** redeem RAVs on-chain to claim payment from the Gateway's escrow

This reduces on-chain transactions from one-per-query to one-per-aggregation-period, drastically lowering costs while maintaining cryptographic guarantees via EIP-712 signatures.

A more detailed specification of the protocol can be found in the following GIPs:
- [GIP-0054](https://github.com/graphprotocol/graph-improvement-proposals/blob/main/gips/0054-timeline-aggregation-protocol.md) - Original TAP specification
- [GIP-0066](https://github.com/graphprotocol/graph-improvement-proposals/blob/main/gips/0066-graph-horizon.md) - Graph Horizon, introducing TAP v2 (renamed to Graph Tally)

## Binaries

These services are run by **Gateway operators**. See each component's README for configuration and deployment details.

| Binary | Description | Docker Image |
|--------|-------------|--------------|
| [graph_tally_aggregator](crates/bin/aggregator/README.md) | Aggregates Receipts into signed RAVs | [![GHCR](https://img.shields.io/github/v/release/graphprotocol/graph-tally?filter=graph_tally_aggregator-*&label=ghcr.io)](https://github.com/graphprotocol/graph-tally/pkgs/container/graph_tally_aggregator) |
| [graph_tally_escrow_manager](crates/bin/escrow_manager/README.md) | Manages escrow balances for Indexer payments | [![GHCR](https://img.shields.io/github/v/release/graphprotocol/graph-tally?filter=graph_tally_escrow_manager-*&label=ghcr.io)](https://github.com/graphprotocol/graph-tally/pkgs/container/graph_tally_escrow_manager) |

## Libraries

| Crate | Version |
|-------|---------|
| [graph_tally_core](crates/core) | [![crates.io](https://img.shields.io/crates/v/graph_tally_core)](https://crates.io/crates/graph_tally_core) |
| [graph_tally_receipt](crates/receipt) | [![crates.io](https://img.shields.io/crates/v/graph_tally_receipt)](https://crates.io/crates/graph_tally_receipt) |
| [graph_tally_graph](crates/graph) | [![crates.io](https://img.shields.io/crates/v/graph_tally_graph)](https://crates.io/crates/graph_tally_graph) |
| [graph_tally_eip712_message](crates/eip712_message) | [![crates.io](https://img.shields.io/crates/v/graph_tally_eip712_message)](https://crates.io/crates/graph_tally_eip712_message) |

## Contributing

Contributions are welcome! Please submit a pull request or open an issue to discuss potential changes. See the [Contributing Guide](CONTRIBUTING.md) for details.
