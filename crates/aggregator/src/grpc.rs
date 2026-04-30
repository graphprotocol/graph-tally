pub mod uint128 {
    tonic::include_proto!("grpc.uint128");

    impl From<Uint128> for u128 {
        fn from(Uint128 { high, low }: Uint128) -> Self {
            ((high as u128) << 64) | low as u128
        }
    }

    impl From<u128> for Uint128 {
        fn from(value: u128) -> Self {
            let high = (value >> 64) as u64;
            let low = value as u64;
            Self { high, low }
        }
    }
}

pub mod v2 {
    use anyhow::anyhow;
    use graph_tally_core::signed_message::Eip712SignedMessage;
    use thegraph_core::alloy::primitives::Bytes;

    tonic::include_proto!("tap_aggregator.v2");

    impl TryFrom<self::Receipt> for graph_tally_graph::Receipt {
        type Error = anyhow::Error;
        fn try_from(receipt: self::Receipt) -> Result<Self, Self::Error> {
            Ok(Self {
                collection_id: receipt.collection_id.as_slice().try_into()?,
                timestamp_ns: receipt.timestamp_ns,
                value: receipt.value.ok_or(anyhow!("Missing value"))?.into(),
                nonce: receipt.nonce,
                payer: receipt.payer.as_slice().try_into()?,
                data_service: receipt.data_service.as_slice().try_into()?,
                service_provider: receipt.service_provider.as_slice().try_into()?,
            })
        }
    }

    impl TryFrom<self::SignedReceipt> for graph_tally_graph::SignedReceipt {
        type Error = anyhow::Error;
        fn try_from(receipt: self::SignedReceipt) -> Result<Self, Self::Error> {
            Ok(Self {
                signature: receipt.signature.as_slice().try_into()?,
                message: receipt
                    .message
                    .ok_or(anyhow!("Missing message"))?
                    .try_into()?,
            })
        }
    }

    impl From<graph_tally_graph::Receipt> for self::Receipt {
        fn from(value: graph_tally_graph::Receipt) -> Self {
            Self {
                collection_id: value.collection_id.as_slice().to_vec(),
                timestamp_ns: value.timestamp_ns,
                nonce: value.nonce,
                value: Some(value.value.into()),
                payer: value.payer.as_slice().to_vec(),
                data_service: value.data_service.as_slice().to_vec(),
                service_provider: value.service_provider.as_slice().to_vec(),
            }
        }
    }

    impl From<graph_tally_graph::SignedReceipt> for self::SignedReceipt {
        fn from(value: graph_tally_graph::SignedReceipt) -> Self {
            Self {
                message: Some(value.message.into()),
                signature: value.signature.as_bytes().to_vec(),
            }
        }
    }

    impl TryFrom<self::SignedRav> for Eip712SignedMessage<graph_tally_graph::ReceiptAggregateVoucher> {
        type Error = anyhow::Error;
        fn try_from(voucher: self::SignedRav) -> Result<Self, Self::Error> {
            Ok(Self {
                signature: voucher.signature.as_slice().try_into()?,
                message: voucher
                    .message
                    .ok_or(anyhow!("Missing message"))?
                    .try_into()?,
            })
        }
    }

    impl From<Eip712SignedMessage<graph_tally_graph::ReceiptAggregateVoucher>> for self::SignedRav {
        fn from(voucher: Eip712SignedMessage<graph_tally_graph::ReceiptAggregateVoucher>) -> Self {
            Self {
                signature: voucher.signature.as_bytes().to_vec(),
                message: Some(voucher.message.into()),
            }
        }
    }

    impl TryFrom<self::ReceiptAggregateVoucher> for graph_tally_graph::ReceiptAggregateVoucher {
        type Error = anyhow::Error;
        fn try_from(voucher: self::ReceiptAggregateVoucher) -> Result<Self, Self::Error> {
            Ok(Self {
                collectionId: voucher.collection_id.as_slice().try_into()?,
                timestampNs: voucher.timestamp_ns,
                valueAggregate: voucher
                    .value_aggregate
                    .ok_or(anyhow!("Missing Value Aggregate"))?
                    .into(),
                payer: voucher.payer.as_slice().try_into()?,
                dataService: voucher.data_service.as_slice().try_into()?,
                serviceProvider: voucher.service_provider.as_slice().try_into()?,
                metadata: Bytes::copy_from_slice(voucher.metadata.as_slice()),
            })
        }
    }

    impl From<graph_tally_graph::ReceiptAggregateVoucher> for self::ReceiptAggregateVoucher {
        fn from(voucher: graph_tally_graph::ReceiptAggregateVoucher) -> Self {
            Self {
                collection_id: voucher.collectionId.to_vec(),
                timestamp_ns: voucher.timestampNs,
                value_aggregate: Some(voucher.valueAggregate.into()),
                payer: voucher.payer.to_vec(),
                data_service: voucher.dataService.to_vec(),
                service_provider: voucher.serviceProvider.to_vec(),
                metadata: voucher.metadata.to_vec(),
            }
        }
    }

    impl self::RavRequest {
        pub fn new(
            receipts: Vec<graph_tally_graph::SignedReceipt>,
            previous_rav: Option<graph_tally_graph::SignedRav>,
        ) -> Self {
            Self {
                receipts: receipts.into_iter().map(Into::into).collect(),
                previous_rav: previous_rav.map(Into::into),
            }
        }
    }

    impl self::RavResponse {
        pub fn signed_rav(mut self) -> anyhow::Result<graph_tally_graph::SignedRav> {
            let signed_rav: graph_tally_graph::SignedRav = self
                .rav
                .take()
                .ok_or(anyhow!("Couldn't find rav"))?
                .try_into()?;
            Ok(signed_rav)
        }
    }
}
