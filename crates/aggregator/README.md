# Graph Tally Aggregator

A stateless JSON-RPC service that lets clients request an aggregate receipt from a list of individual receipts.

Graph Tally Aggregator is run by [gateway](https://github.com/edgeandnode/gateway/blob/main/README.md)
operators.

As described in the [gateway README section on Graph Tally](https://github.com/edgeandnode/gateway/blob/main/README.md#tap):

> The `gateway` acts as a Graph Tally sender, where each indexer request is sent with a receipt. The `gateway` operator is expected to run 2 additional services:
>
> - `graph_tally_aggregator` (this crate!): public endpoint where indexers can aggregate receipts into RAVs
> - [tap-escrow-manager](https://github.com/edgeandnode/tap-escrow-manager): maintains escrow balances for the sender. This service requires data exported by the gateway into the "indexer requests" topic to calculate the value of outstanding receipts to each indexer.
>
> The `gateway` operator is also expected to manage at least 2 wallets:
>
> - sender: requires ETH for transaction gas and GRT to allocate into escrow balances for paying indexers
> - authorized signer: used by the `gateway` and `graph_tally_aggregator` to sign receipts and RAVs

## Settings

```txt
A JSON-RPC service for Graph Tally that lets clients request an aggregate receipt from a list of
individual receipts.

Usage: graph_tally_aggregator [OPTIONS] --private-key <PRIVATE_KEY>

Options:
      --port <PORT>
          Port to listen on for JSON-RPC requests [env: GRAPH_TALLY_PORT=] [default: 8080]
      --private-key <PRIVATE_KEY>
          Sender private key for signing Receipt Aggregate Vouchers, as a hex string [env: GRAPH_TALLY_PRIVATE_KEY=]
      --public-keys <PUBLIC_KEYS>
          Signer public keys for incoming receipts/RAVs [env: GRAPH_TALLY_PUBLIC_KEYS=]
      --max-request-body-size <MAX_REQUEST_BODY_SIZE>
          Maximum request body size in bytes. Defaults to 10MB [env: GRAPH_TALLY_MAX_REQUEST_BODY_SIZE=] [default: 10485760]
      --max-response-body-size <MAX_RESPONSE_BODY_SIZE>
          Maximum response body size in bytes. Defaults to 100kB [env: GRAPH_TALLY_MAX_RESPONSE_BODY_SIZE=] [default: 102400]
      --max-connections <MAX_CONNECTIONS>
          Maximum number of concurrent connections. Defaults to 32 [env: GRAPH_TALLY_MAX_CONNECTIONS=] [default: 32]
      --request-timeout-secs <REQUEST_TIMEOUT_SECS>
          Maximum time in seconds allowed for processing a request. This timeout protects against
          Slowloris-style DoS attacks by ensuring that connections cannot be held open indefinitely.
          Defaults to 60 seconds [env: GRAPH_TALLY_REQUEST_TIMEOUT_SECS=] [default: 60]
      --metrics-port <METRICS_PORT>
          Metrics server port [env: GRAPH_TALLY_METRICS_PORT=] [default: 5000]
      --domain-chain-id <DOMAIN_CHAIN_ID>
          Domain chain ID for EIP-712 domain separator [env: GRAPH_TALLY_DOMAIN_CHAIN_ID=]
      --domain-verifying-contract <DOMAIN_VERIFYING_CONTRACT>
          Domain verifying contract for EIP-712 domain separator [env: GRAPH_TALLY_DOMAIN_VERIFYING_CONTRACT=]
      --kafka-config <KAFKA_CONFIG>
          Kafka configuration [env: GRAPH_TALLY_KAFKA_CONFIG=]
  -h, --help
          Print help
  -V, --version
          Print version
```

Please refer to [GraphTallyCollector](https://github.com/graphprotocol/contracts/blob/main/packages/horizon/contracts/payments/collectors/GraphTallyCollector.sol) for more information about Receipt Aggregate Voucher signing keys.

### Deprecated Environment Variables

For backwards compatibility, the following `TAP_*` environment variables are still supported but deprecated:

| Deprecated | Use Instead |
|------------|-------------|
| `TAP_PORT` | `GRAPH_TALLY_PORT` |
| `TAP_PRIVATE_KEY` | `GRAPH_TALLY_PRIVATE_KEY` |
| `TAP_PUBLIC_KEYS` | `GRAPH_TALLY_PUBLIC_KEYS` |
| `TAP_MAX_REQUEST_BODY_SIZE` | `GRAPH_TALLY_MAX_REQUEST_BODY_SIZE` |
| `TAP_MAX_RESPONSE_BODY_SIZE` | `GRAPH_TALLY_MAX_RESPONSE_BODY_SIZE` |
| `TAP_MAX_CONNECTIONS` | `GRAPH_TALLY_MAX_CONNECTIONS` |
| `TAP_REQUEST_TIMEOUT_SECS` | `GRAPH_TALLY_REQUEST_TIMEOUT_SECS` |
| `TAP_METRICS_PORT` | `GRAPH_TALLY_METRICS_PORT` |
| `TAP_DOMAIN_CHAIN_ID` | `GRAPH_TALLY_DOMAIN_CHAIN_ID` |
| `TAP_DOMAIN_VERIFYING_CONTRACT` | `GRAPH_TALLY_DOMAIN_VERIFYING_CONTRACT` |
| `TAP_KAFKA_CONFIG` | `GRAPH_TALLY_KAFKA_CONFIG` |

## Operational recommendations

This is just meant to be a non-exhaustive list of reminders for safely operating the Graph Tally Aggregator. It being an HTTP
service, use your best judgement and apply the industry-standard best practices when serving HTTP to the public
internet.

- Advertise through a safe DNS service (w/ DNSSEC, etc)
- Expose through HTTPS only (by reverse-proxying)
- Use a WAF, to leverage (if available):
  - DDoS protection, rate limiting, etc.
  - Geofencing, depending on the operator's jurisdiction.
  - HTTP response inspection.
  - JSON request and response inspection. To validate the inputs, as well as parse JSON-RPC error codes in the response.

It is also recommended that clients use HTTP compression for their HTTP requests to the Graph Tally Aggregator, as RAV requests
can be quite large.

## JSON-RPC API

### Common interface

#### Request format

The request format is standard, as described in
[the official spec](https://www.jsonrpc.org/specification#request_object).

#### Successful response format

If the call is successful, the response format is as described in
[the official spec](https://www.jsonrpc.org/specification#response_object), and in addition the `result` field is of the
form:

```json
{
    "id": 0,
    "jsonrpc": "2.0",
    "result": {
        "data": {...},
        "warnings": [
            {
                "code": -32000,
                "message": "Error message",
                "data": {...}
            }
        ]
    }
}
```

| Field      | Type     | Description                                                                                              |
| ---------- | -------- | -------------------------------------------------------------------------------------------------------- |
| `data`     | `Object` | The response data. Method specific, see each method's documentation.                                     |
| `warnings` | `Array`  | (Optional) A list of warnings. If the list is empty, no warning field is added to the JSON-RPC response. |

WARNING: Always check for warnings!

Warning object format (similar to the standard JSON-RPC error object):

| Field     | Type      | Description                                                                                      |
| --------- | --------- | ------------------------------------------------------------------------------------------------ |
| `code`    | `Integer` | A number that indicates the error type that occurred.                                            |
| `message` | `String`  | A short description of the error.                                                                |
| `data`    | `Object`  | (Optional) A primitive or structured value that contains additional information about the error. |

We define these warning codes:

- `-32051` API version deprecation

  Also returns an object containing the method's supported versions in the `data` field. Example:

  ```json
  {
      "id": 0,
      "jsonrpc": "2.0",
      "result": {
          "data": {...},
          "warnings": [
              {
                  "code": -32051,
                  "data": {
                      "versions_deprecated": [
                          "0.0"
                      ],
                      "versions_supported": [
                          "0.0",
                          "0.1"
                      ]
                  },
                  "message": "The API version 0.0 will be deprecated. Please check https://github.com/graphprotocol/graph-tally for more information."
              }
          ]
      }
  }
  ```

#### Error response format

If the call fails, the error response format is as described in
[the official spec](https://www.jsonrpc.org/specification#error_object).

In addition to the official spec, we define a few special errors:

- `-32001` Invalid API version.

  Also returns an object containing the method's supported versions in the `data` field. Example:

  ```json
  {
      "error": {
          "code": -32001,
          "data": {
              "versions_deprecated": [
                  "0.0"
              ],
              "versions_supported": [
                  "0.0",
                  "0.1"
              ]
          },
          "message": "Unsupported API version: \"0.2\"."
      },
      "id": 0,
      "jsonrpc": "2.0"
  }
  ```

- `-32002` Aggregation error.

  The aggregation function returned an error. Example:

  ```json
  {
      "error": {
          "code": -32002,
          "message": "Signature verification failed. Expected 0x9858…da94, got 0x3ef9…a4a3"
      },
      "id": 0,
      "jsonrpc": "2.0"
  }
  ```

### Methods

#### `api_versions()`

[source](server::RpcServer::api_versions)

Returns the versions of the Graph Tally JSON-RPC API implemented by this server.

Example:

*Request*:

```json
{
    "jsonrpc": "2.0",
    "id": 0,
    "method": "api_versions",
    "params": [
        null
    ]
}
```

*Response*:

```json
{
    "id": 0,
    "jsonrpc": "2.0",
    "result": {
        "data": {
            "versions_deprecated": [
               "0.0"
            ],
            "versions_supported": [
                "0.0",
                "0.1"
            ]
        }
    }
}
```

#### `aggregate_receipts(api_version, receipts, previous_rav)`

[source](server::RpcServer::aggregate_receipts)

Aggregates the given receipts into a receipt aggregate voucher.
Returns an error if the user expected API version is not supported.

We recommend that the server is set-up to support a maximum HTTP request size of 10MB, in which case we guarantee that
`aggregate_receipts` support a maximum of at least 15,000 receipts per call. If you have more than 15,000 receipts to
aggregate, we recommend calling `aggregate_receipts` multiple times.

**Receipt Structure:**

- `collection_id`: 32-byte identifier for the collection
- `payer`: Address of the payer
- `data_service`: Address of the data service
- `service_provider`: Address of the service provider
- `timestamp_ns`: Timestamp in nanoseconds
- `nonce`: Unique nonce
- `value`: Receipt value

**RAV Structure:**

- `collectionId`: Collection identifier
- `payer`: Payer address
- `dataService`: Data service address
- `serviceProvider`: Service provider address
- `timestampNs`: Latest timestamp
- `valueAggregate`: Total aggregated value
- `metadata`: Additional metadata (bytes)

Example:

*Request*:

```json
{
  "jsonrpc": "2.0",
  "id": 0,
  "method": "aggregate_receipts",
  "params": [
    "0.0",
    [
      {
        "message": {
          "collection_id": "0xdeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddead",
          "payer": "0xabababababababababababababababababababab",
          "data_service": "0xdeaddeaddeaddeaddeaddeaddeaddeaddeaddead",
          "service_provider": "0xbeefbeefbeefbeefbeefbeefbeefbeefbeefbeef",
          "timestamp_ns": 1685670449225087255,
          "nonce": 11835827017881841442,
          "value": 34
        },
        "signature": {
          "r": "0xa9fa1acf3cc3be503612f75602e68cc22286592db1f4f944c78397cbe529353b",
          "s": "0x566cfeb7e80a393021a443d5846c0734d25bcf54ed90d97effe93b1c8aef0911",
          "v": 27
        }
      },
      {
        "message": {
          "collection_id": "0xdeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddead",
          "payer": "0xabababababababababababababababababababab",
          "data_service": "0xdeaddeaddeaddeaddeaddeaddeaddeaddeaddead",
          "service_provider": "0xbeefbeefbeefbeefbeefbeefbeefbeefbeefbeef",
          "timestamp_ns": 1685670449225830106,
          "nonce": 17711980309995246801,
          "value": 23
        },
        "signature": {
          "r": "0x51ca5a2b839558654326d3a3f544a97d94effb9a7dd9cac7492007bc974e91f0",
          "s": "0x3d9d398ea6b0dd9fac97726f51c0840b8b314821fb4534cb40383850c431fd9e",
          "v": 28
        }
      }
    ],
    {
      "message": {
        "collectionId": "0xdeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddead",
        "payer": "0xabababababababababababababababababababab",
        "dataService": "0xdeaddeaddeaddeaddeaddeaddeaddeaddeaddead",
        "serviceProvider": "0xbeefbeefbeefbeefbeefbeefbeefbeefbeefbeef",
        "timestampNs": 1685670449224324338,
        "valueAggregate": 101,
        "metadata": "0x"
      },
      "signature": {
        "r": "0x601a1f399cf6223d1414a89b7bbc90ee13eeeec006bd59e0c96042266c6ad7dc",
        "s": "0x3172e795bd190865afac82e3a8be5f4ccd4b65958529986c779833625875f0b2",
        "v": 28
      }
    }
  ]
}
```

*Response*:

```json
{
  "id": 0,
  "jsonrpc": "2.0",
  "result": {
    "data": {
      "message": {
        "collectionId": "0xdeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddead",
        "payer": "0xabababababababababababababababababababab",
        "dataService": "0xdeaddeaddeaddeaddeaddeaddeaddeaddeaddead",
        "serviceProvider": "0xbeefbeefbeefbeefbeefbeefbeefbeefbeefbeef",
        "timestampNs": 1685670449225830106,
        "valueAggregate": 158,
        "metadata": "0x"
      },
      "signature": {
        "r": "0x60eb38374119bbabf1ac6960f532124ba2a9c5990d9fb50875b512e611847eb5",
        "s": "0x1b9a330cc9e2ecbda340a4757afaee8f55b6dbf278428f8cf49dd5ad8438f83d",
        "v": 27
      }
    }
  }
}
```

## Metrics

The aggregator exposes Prometheus metrics for monitoring. Key metrics include:

| Metric | Type | Description |
|--------|------|-------------|
| `aggregation_success_count` | Counter | Number of successful receipt aggregation requests |
| `aggregation_failure_count` | Counter | Number of failed receipt aggregation requests |
| `total_aggregated_receipts` | Counter | Total number of receipts successfully aggregated |
| `total_aggregated_grt` | Counter | Total successfully aggregated GRT value (wei) |
| `kafka_publish_success_total` | Counter | Number of successful Kafka publish attempts for RAV records |
| `kafka_publish_failure_total` | Counter | Number of failed Kafka publish attempts for RAV records |

### Kafka Failure Handling

When Kafka publishing fails, the aggregator:
- Increments `kafka_publish_failure_total`
- Logs an error with context about potential escrow tracking drift
- **Still returns the RAV to the client** (availability over consistency)

Operators should alert on `kafka_publish_failure_total > 0` to detect Kafka connectivity issues. If Kafka messages are lost, `tap-escrow-manager` may underestimate debt—see the `debts` config override in tap-escrow-manager for manual recovery.
