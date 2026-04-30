use std::{
    collections::HashMap,
    str::FromStr,
    sync::{Arc, RwLock},
};

use rstest::*;
use tap_core::{
    manager::{
        adapters::{RavRead, RavStore},
        context::memory::InMemoryContext,
    },
    receipt::checks::StatefulTimestampCheck,
    signed_message::Eip712SignedMessage,
    tap_eip712_domain,
};
use tap_graph::{Receipt, ReceiptAggregateVoucher};
#[allow(deprecated)]
use thegraph_core::alloy::primitives::{Address, FixedBytes, Signature, B256};
use thegraph_core::alloy::{dyn_abi::Eip712Domain, signers::local::PrivateKeySigner};

#[fixture]
fn domain_separator() -> Eip712Domain {
    tap_eip712_domain(1, Address::from([0x11u8; 20]))
}

#[fixture]
fn context() -> InMemoryContext {
    let escrow_storage = Arc::new(RwLock::new(HashMap::new()));
    let rav_storage = Arc::new(RwLock::new(None));
    let receipt_storage = Arc::new(RwLock::new(HashMap::new()));

    let timestamp_check = Arc::new(StatefulTimestampCheck::new(0));
    InMemoryContext::new(
        rav_storage,
        receipt_storage.clone(),
        escrow_storage.clone(),
        timestamp_check,
    )
}

#[fixture]
fn collection_id() -> FixedBytes<32> {
    FixedBytes::from([0xab; 32])
}

#[fixture]
fn payer() -> Address {
    Address::from_str("0xabababababababababababababababababababab").unwrap()
}

#[fixture]
fn data_service() -> Address {
    Address::from_str("0xdeaddeaddeaddeaddeaddeaddeaddeaddeaddead").unwrap()
}

#[fixture]
fn service_provider() -> Address {
    Address::from_str("0xbeefbeefbeefbeefbeefbeefbeefbeefbeefbeef").unwrap()
}

#[rstest]
fn check_for_rav_serialization(
    domain_separator: Eip712Domain,
    collection_id: FixedBytes<32>,
    payer: Address,
    data_service: Address,
    service_provider: Address,
) {
    // Use a deterministic private key for reproducible signatures
    let private_key = B256::repeat_byte(0x01);
    let wallet = PrivateKeySigner::from_bytes(&private_key).unwrap();
    let mut receipts = Vec::new();
    for value in 50..60 {
        receipts.push(
            Eip712SignedMessage::new(
                &domain_separator,
                Receipt {
                    collection_id,
                    value,
                    nonce: value as u64,
                    timestamp_ns: value as u64,
                    payer,
                    data_service,
                    service_provider,
                },
                &wallet,
            )
            .unwrap(),
        );
    }

    let signed_rav = Eip712SignedMessage::new(
        &domain_separator,
        ReceiptAggregateVoucher::aggregate_receipts(
            collection_id,
            payer,
            data_service,
            service_provider,
            &receipts,
            None,
        )
        .unwrap(),
        &wallet,
    )
    .unwrap();

    insta::assert_json_snapshot!(receipts);
    insta::assert_json_snapshot!(signed_rav);

    let raw_sig = r#"{
      "r": "0x1596dd0d380ede7aa5dec5ed09ea7d1fa8e4bc8dfdb43a4e965bb4f16906e321",
      "s": "0x788b69625a031fbd2e769928b63505387df16e7c51f19ff67c782bfec101a387",
      "yParity": "0x1"
    }"#;

    serde_json::from_str::<Signature>(raw_sig).unwrap();
    #[allow(deprecated)]
    serde_json::from_str::<Signature>(raw_sig).unwrap();
}

#[rstest]
#[tokio::test]
async fn rav_storage_adapter_test(
    domain_separator: Eip712Domain,
    context: InMemoryContext,
    collection_id: FixedBytes<32>,
    payer: Address,
    data_service: Address,
    service_provider: Address,
) {
    let wallet = PrivateKeySigner::random();

    // Create receipts
    let mut receipts = Vec::new();
    for value in 50..60 {
        receipts.push(
            Eip712SignedMessage::new(
                &domain_separator,
                Receipt::new(collection_id, payer, data_service, service_provider, value).unwrap(),
                &wallet,
            )
            .unwrap(),
        );
    }

    let signed_rav = Eip712SignedMessage::new(
        &domain_separator,
        ReceiptAggregateVoucher::aggregate_receipts(
            collection_id,
            payer,
            data_service,
            service_provider,
            &receipts,
            None,
        )
        .unwrap(),
        &wallet,
    )
    .unwrap();

    context.update_last_rav(signed_rav.clone()).await.unwrap();

    // Retreive rav
    let retrieved_rav = context.last_rav().await;
    assert!(retrieved_rav.unwrap().unwrap() == signed_rav);

    // Testing the last rav update...

    // Create more receipts
    let mut receipts = Vec::new();
    for value in 60..70 {
        receipts.push(
            Eip712SignedMessage::new(
                &domain_separator,
                Receipt::new(collection_id, payer, data_service, service_provider, value).unwrap(),
                &wallet,
            )
            .unwrap(),
        );
    }

    let signed_rav = Eip712SignedMessage::new(
        &domain_separator,
        ReceiptAggregateVoucher::aggregate_receipts(
            collection_id,
            payer,
            data_service,
            service_provider,
            &receipts,
            None,
        )
        .unwrap(),
        &wallet,
    )
    .unwrap();

    // Update the last rav
    context.update_last_rav(signed_rav.clone()).await.unwrap();

    // Retreive rav
    let retrieved_rav = context.last_rav().await;
    assert!(retrieved_rav.unwrap().unwrap() == signed_rav);
}
