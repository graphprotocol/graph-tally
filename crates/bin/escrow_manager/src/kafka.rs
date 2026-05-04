pub use ravs::ravs;
use rdkafka::consumer::StreamConsumer;
pub use receipts::receipts;

use crate::config;

fn consumer(config: &config::Kafka) -> anyhow::Result<StreamConsumer> {
    let mut consumer_config = rdkafka::ClientConfig::from_iter(config.config.clone());
    let defaults = [
        ("group.id", "graph-tally-escrow-manager"),
        ("enable.auto.commit", "true"),
        ("enable.auto.offset.store", "true"),
    ];
    for (key, value) in defaults {
        if !consumer_config.config_map().contains_key(key) {
            consumer_config.set(key, value);
        }
    }
    Ok(consumer_config.create()?)
}

mod receipts {
    use std::collections::BTreeMap;

    use alloy::{hex::ToHexExt as _, primitives::Address};
    use anyhow::{anyhow, Context as _};
    use chrono::{DateTime, Duration, Utc};
    use futures_util::StreamExt as _;
    use prost::Message as _;
    use rdkafka::{
        consumer::{Consumer as _, StreamConsumer},
        Message as _,
    };
    use titorelli::kafka::{assign_partitions, latest_messages};
    use tokio::sync::{mpsc, watch};

    use super::consumer;
    use crate::config;

    pub async fn receipts(
        config: &config::Kafka,
        signers: Vec<Address>,
    ) -> anyhow::Result<watch::Receiver<BTreeMap<Address, u128>>> {
        let window = Duration::days(28);
        let (tx, rx) = watch::channel(Default::default());
        let db = DB::spawn(window, tx);
        let mut consumer = consumer(config)?;

        let start_timestamp = hourly_timestamp(Utc::now() - window);
        if let Some(aggregated_topic) = &config.aggregated_topic {
            let latest_aggregated_messages =
                latest_messages(&consumer, &[aggregated_topic]).await?;
            let mut latest_aggregated_offsets: BTreeMap<String, i64> = latest_aggregated_messages
                .into_iter()
                .map(|msg| (format!("{}/{}", msg.topic(), msg.partition()), msg.offset()))
                .collect();
            assign_partitions(&consumer, &[aggregated_topic], start_timestamp).await?;
            let mut latest_aggregated_timestamp = 0;
            let mut stream = consumer.stream();
            while let Some(msg) = stream.next().await {
                let msg = msg?;
                let partition = format!("{}/{}", msg.topic(), msg.partition());
                let offset = msg.offset();
                let payload = msg
                    .payload()
                    .with_context(|| anyhow!("missing payload at {partition} {offset}"))?;
                let msg = IndexerFeesHourlyProtobuf::decode(payload)?;
                latest_aggregated_timestamp = latest_aggregated_timestamp.max(msg.timestamp);
                if let Some(cutoff) = config.aggregated_cutoff_timestamp {
                    if msg.timestamp < cutoff {
                        continue;
                    }
                }
                for aggregation in &msg.aggregations {
                    if !signers.contains(&Address::from_slice(&aggregation.signer)) {
                        continue;
                    }
                    let update = Update {
                        timestamp: DateTime::from_timestamp_millis(msg.timestamp)
                            .context("timestamp out of range")?,
                        indexer: Address::from_slice(&aggregation.receiver),
                        fee: (aggregation.fee_grt * 1e18) as u128,
                    };
                    db.send(update).await.unwrap();
                }

                if latest_aggregated_offsets.get(&partition).unwrap() == &offset {
                    latest_aggregated_offsets.remove(&partition);
                    if latest_aggregated_offsets.is_empty() {
                        break;
                    }
                }
            }
            consumer.unassign()?;
            let realtime_start =
                latest_aggregated_timestamp + Duration::hours(1).num_milliseconds();
            assign_partitions(&consumer, &[&config.realtime_topic], realtime_start).await?;
        } else {
            assign_partitions(&consumer, &[&config.realtime_topic], start_timestamp).await?;
        }
        tokio::spawn(async move {
            if let Err(kafka_consumer_err) = process_messages(&mut consumer, db, signers).await {
                tracing::error!(%kafka_consumer_err);
            }
        });

        Ok(rx)
    }

    #[derive(prost::Message)]
    struct IndexerFeesProtobuf {
        /// 20 bytes (address)
        #[prost(bytes, tag = "1")]
        signer: Vec<u8>,
        /// 20 bytes (address)
        #[prost(bytes, tag = "2")]
        receiver: Vec<u8>,
        #[prost(double, tag = "3")]
        fee_grt: f64,
    }

    #[derive(prost::Message)]
    struct IndexerFeesHourlyProtobuf {
        /// start timestamp for aggregation, in unix milliseconds
        #[prost(int64, tag = "1")]
        timestamp: i64,
        #[prost(message, repeated, tag = "2")]
        aggregations: Vec<IndexerFeesProtobuf>,
    }

    #[derive(prost::Message)]
    struct ClientQueryProtobuf {
        // 20 bytes (address)
        #[prost(bytes, tag = "2")]
        receipt_signer: Vec<u8>,
        #[prost(message, repeated, tag = "10")]
        indexer_queries: Vec<IndexerQueryProtobuf>,
    }
    #[derive(prost::Message)]
    struct IndexerQueryProtobuf {
        /// 20 bytes (address)
        #[prost(bytes, tag = "1")]
        indexer: Vec<u8>,
        #[prost(double, tag = "6")]
        fee_grt: f64,
    }

    async fn process_messages(
        consumer: &mut StreamConsumer,
        db: mpsc::Sender<Update>,
        signers: Vec<Address>,
    ) -> anyhow::Result<()> {
        consumer
            .stream()
            .for_each_concurrent(16, |msg| async {
                let msg = match msg {
                    Ok(msg) => msg,
                    Err(recv_error) => {
                        tracing::error!(%recv_error);
                        return;
                    }
                };
                let payload = match msg.payload() {
                    Some(payload) => payload,
                    None => return,
                };
                let timestamp = msg
                    .timestamp()
                    .to_millis()
                    .and_then(|t| DateTime::from_timestamp(t / 1_000, (t % 1_000) as u32 * 1_000))
                    .unwrap_or_else(Utc::now);
                let payload = match ClientQueryProtobuf::decode(payload) {
                    Ok(payload) => payload,
                    Err(payload_parse_err) => {
                        tracing::error!(%payload_parse_err, input = payload.encode_hex());
                        return;
                    }
                };
                if !signers.contains(&Address::from_slice(&payload.receipt_signer)) {
                    return;
                }
                for indexer_query in payload.indexer_queries {
                    let update = Update {
                        timestamp,
                        indexer: Address::from_slice(&indexer_query.indexer),
                        fee: (indexer_query.fee_grt * 1e18) as u128,
                    };
                    let _ = db.send(update).await;
                }
            })
            .await;
        Ok(())
    }

    pub struct Update {
        pub timestamp: DateTime<Utc>,
        pub indexer: Address,
        pub fee: u128,
    }

    pub struct DB {
        // indexer debts, aggregated per hour
        data: BTreeMap<Address, BTreeMap<i64, u128>>,
        window: Duration,
        tx: watch::Sender<BTreeMap<Address, u128>>,
    }

    impl DB {
        pub fn spawn(
            window: Duration,
            tx: watch::Sender<BTreeMap<Address, u128>>,
        ) -> mpsc::Sender<Update> {
            let mut db = Self {
                data: Default::default(),
                window,
                tx,
            };
            let (tx, mut rx) = mpsc::channel(128);
            tokio::spawn(async move {
                let mut last_snapshot = Utc::now();
                let buffer_size = 128;
                let mut buffer: Vec<Update> = Vec::with_capacity(buffer_size);
                loop {
                    rx.recv_many(&mut buffer, buffer_size).await;
                    let now = Utc::now();
                    for update in buffer.drain(..) {
                        db.update(update, now);
                    }

                    if (now - last_snapshot) >= Duration::seconds(1) {
                        db.prune(now);
                        let snapshot = db.snapshot();

                        let _ = db.tx.send(snapshot);
                        last_snapshot = now;
                    }
                }
            });
            tx
        }

        fn update(&mut self, update: Update, now: DateTime<Utc>) {
            if update.timestamp < (now - self.window) {
                return;
            }
            let entry = self
                .data
                .entry(update.indexer)
                .or_default()
                .entry(hourly_timestamp(update.timestamp))
                .or_default();
            *entry += update.fee;
        }

        fn prune(&mut self, now: DateTime<Utc>) {
            let min_timestamp = hourly_timestamp(now - self.window);
            self.data.retain(|_, entries| {
                entries.retain(|t, _| *t > min_timestamp);
                !entries.is_empty()
            });
        }

        fn snapshot(&self) -> BTreeMap<Address, u128> {
            self.data
                .iter()
                .map(|(indexer, entries)| (*indexer, entries.values().sum()))
                .collect()
        }
    }

    fn hourly_timestamp(t: DateTime<Utc>) -> i64 {
        let t = t.timestamp();
        t - (t % Duration::hours(1).num_seconds())
    }
}

mod ravs {
    use std::collections::BTreeMap;

    use alloy::primitives::Address;
    use anyhow::Context as _;
    use futures_util::StreamExt as _;
    use rdkafka::{consumer::StreamConsumer, Message as _};
    use titorelli::kafka::assign_partitions;
    use tokio::sync::watch;

    use super::consumer;
    use crate::config;

    pub async fn ravs(
        config: &config::Kafka,
        signers: Vec<Address>,
    ) -> anyhow::Result<watch::Receiver<BTreeMap<Address, u128>>> {
        let (tx, rx) = watch::channel(Default::default());
        let mut consumer = consumer(config)?;
        assign_partitions(&consumer, &["gateway_ravs"], 0).await?;
        tokio::spawn(async move { process_messages(&mut consumer, tx, signers).await });
        Ok(rx)
    }

    async fn process_messages(
        consumer: &mut StreamConsumer,
        tx: watch::Sender<BTreeMap<Address, u128>>,
        signers: Vec<Address>,
    ) {
        consumer
            .stream()
            .for_each_concurrent(16, |msg| async {
                let msg = match msg {
                    Ok(msg) => msg,
                    Err(recv_error) => {
                        tracing::error!(%recv_error);
                        return;
                    }
                };
                let record = match parse_record(&msg) {
                    Ok(ParseResult::V2(record)) => record,
                    Ok(ParseResult::V1) => return,
                    Err(record_parse_err) => {
                        let key = msg.key().map(String::from_utf8_lossy);
                        let payload = msg.payload().map(String::from_utf8_lossy);
                        tracing::error!(%record_parse_err, ?key, ?payload);
                        return;
                    }
                };
                if !signers.contains(&record.signer) {
                    return;
                }
                tx.send_if_modified(|map| {
                    match map.entry(record.allocation) {
                        std::collections::btree_map::Entry::Vacant(entry) => {
                            entry.insert(record.value);
                        }
                        std::collections::btree_map::Entry::Occupied(mut entry)
                            if *entry.get() < record.value =>
                        {
                            entry.insert(record.value);
                        }
                        _ => return false,
                    };
                    true
                });
            })
            .await;
    }

    struct Record {
        signer: Address,
        allocation: Address,
        value: u128,
    }

    enum ParseResult {
        V2(Record),
        V1,
    }

    fn parse_record(msg: &rdkafka::message::BorrowedMessage) -> anyhow::Result<ParseResult> {
        let key = String::from_utf8_lossy(msg.key().context("missing key")?);
        let payload = String::from_utf8_lossy(msg.payload().context("missing payload")?);
        let (signer, id) = key.split_once(':').context("malformed key")?;
        // V1: allocation ID is 20 bytes (42 chars with 0x prefix)
        // V2: collection ID is 32 bytes (66 chars with 0x prefix)
        if id.len() == 42 {
            return Ok(ParseResult::V1);
        }
        anyhow::ensure!(id.len() == 66, "invalid id length: {}", id.len());
        // Allocation ID is the last 20 bytes of collection ID
        let allocation = &id[26..]; // skip "0x" + 24 zero chars (12 bytes padding)
        Ok(ParseResult::V2(Record {
            signer: signer.parse()?,
            allocation: allocation.parse()?,
            value: payload.parse()?,
        }))
    }
}
