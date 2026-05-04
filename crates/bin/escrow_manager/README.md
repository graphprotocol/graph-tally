# Graph Tally Escrow Manager

This service maintains Graph Tally escrow balances on behalf of a gateway sender.

The following data sources are monitored to guide the allocation of GRT into the [Graph Horizon Escrow contract](https://github.com/graphprotocol/contracts/blob/main/packages/horizon/contracts/payments/PaymentsEscrow.sol):

- [Graph Network Subgraph](https://github.com/graphprotocol/graph-network-subgraph) - for active allocations, escrow accounts, and authorized signers
- Kafka topics for receipts and RAVs - to track outstanding debts from query fees

# Configuration

Configuration options are set via a single JSON file. The structure of the file is defined in [src/config.rs](src/config.rs).

## Key Options

| Field | Description |
|-------|-------------|
| `authorize_signers` | If `true`, automatically authorize signers on startup |
| `dry_run` | If `true`, skip contract calls (useful for testing) |
| `port_metrics` | Port for Prometheus metrics server (default: 9090) |
| `update_interval_seconds` | Polling interval for the main loop |

## Sender and Signers

The sender address used for graph_tally_escrow_manager expects authorizedSigners:

- **Sender**: Requires ETH for transaction gas and GRT to allocate into Graph Tally escrow balances for paying indexers
- **Authorized signer**: Used by the gateway and graph_tally_aggregator to sign receipts and RAVs

When `authorize_signers` is set to `true`, the graph_tally_escrow_manager will automatically setup authorized signers on startup. This requires the secret keys for the authorized signer wallets to be present in the `signers` config field.

## Setting up Authorized Signers Manually

To set up authorized signers for graph_tally_escrow_manager:

1. [Find the `PaymentsEscrow` contract address](https://github.com/graphprotocol/contracts/blob/main/packages/horizon/addresses.json) for your network.
2. Navigate to the relevant blockchain explorer (e.g., https://arbiscan.io/address/0xf6Fcc27aAf1fcD8B254498c9794451d82afC673E).
3. Connect the sender address (the address graph_tally_escrow_manager is running with, or the address whose private key you provide in the `secret_key` field of `graph_tally_escrow_manager` config).
4. Go to the "Write Contract" tab and find the `authorizeSigner` function.
5. Generate proof and proofDeadline using the script below.

```bash
mkdir proof-generator && cd proof-generator
npm init -y
npm install ethers
cat > generateProof.js << EOL
const ethers = require('ethers');

async function generateProof(signerPrivateKey, proofDeadline, senderAddress, chainId) {
    const signer = new ethers.Wallet(signerPrivateKey);

    const messageHash = ethers.solidityPackedKeccak256(
        ['uint256', 'uint256', 'address'],
        [chainId, proofDeadline, senderAddress]
    );

    const digest = ethers.hashMessage(ethers.getBytes(messageHash));
    const signature = await signer.signMessage(ethers.getBytes(messageHash));

    return signature;
}

const signerPrivateKey = process.argv[2];
const senderAddress = process.argv[3];
const chainId = parseInt(process.argv[4]);
const proofDeadline = Math.floor(Date.now() / 1000) + 3600; // 1 hour from now

if (!signerPrivateKey || !senderAddress || !chainId) {
    console.error('Usage: node generateProof.js <signerPrivateKey> <senderAddress> <chainId>');
    process.exit(1);
}

generateProof(signerPrivateKey, proofDeadline, senderAddress, chainId)
    .then(proof => {
        console.log('Proof:', proof);
        console.log('ProofDeadline:', proofDeadline);
        console.log('Human-readable date:', new Date(proofDeadline * 1000).toUTCString());
        console.log('Chain ID:', chainId);
    })
    .catch(error => console.error('Error:', error));
EOL

echo "Setup complete. Run the script with:"
echo "node generateProof.js <authorizedSignerPrivateKey> <senderAddress> <chainId>"
```

6. Pass signerAddress, proofDeadline, and proof to the contract and sign the transaction. Repeat if using multiple authorisedSigners

# Logs

Log levels are controlled by the `RUST_LOG` environment variable ([details](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html)).

Example: `RUST_LOG=info,graph_tally_escrow_manager=debug cargo run -- config.json`

# Metrics

Prometheus metrics are exposed on a separate HTTP server. Configure the port via `port_metrics` in the config file (default: 9090).

```bash
curl http://localhost:9090/metrics
```

### Available Metrics

| Metric | Type | Description |
|--------|------|-------------|
| `escrow_total_debt_grt` | Gauge | Total outstanding debt across all receivers |
| `escrow_total_balance_grt` | Gauge | Total escrow balance across all receivers |
| `escrow_total_adjustment_grt` | Gauge | Total GRT deposited in the last cycle |
| `escrow_receiver_count` | Gauge | Number of receivers being tracked |
| `escrow_loop_duration_seconds` | Histogram | Duration of each polling cycle |
| `escrow_debt_grt{receiver}` | Gauge | Outstanding debt per receiver |
| `escrow_balance_grt{receiver}` | Gauge | Escrow balance per receiver |
| `escrow_adjustment_grt{receiver}` | Gauge | Last adjustment per receiver |
| `escrow_deposit_ok` | Counter | Successful deposit transactions |
| `escrow_deposit_err` | Counter | Failed deposit transactions |
| `escrow_deposit_duration` | Histogram | Deposit transaction duration |
