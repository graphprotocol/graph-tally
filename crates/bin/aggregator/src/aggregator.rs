use std::collections::HashSet;

use anyhow::{anyhow, bail, Ok, Result};
use graph_tally_core::{receipt::WithUniqueId, signed_message::Eip712SignedMessage};
use graph_tally_graph::{Receipt, ReceiptAggregateVoucher};
use rayon::prelude::*;
use thegraph_core::alloy::{
    dyn_abi::Eip712Domain,
    primitives::{Address, FixedBytes},
    sol_types::SolStruct,
};

use crate::signers::SignerRegistry;

pub fn check_and_aggregate_receipts(
    domain_separator: &Eip712Domain,
    receipts: &[Eip712SignedMessage<Receipt>],
    previous_rav: Option<Eip712SignedMessage<ReceiptAggregateVoucher>>,
    signers: &SignerRegistry,
) -> Result<Eip712SignedMessage<ReceiptAggregateVoucher>> {
    check_signatures_unique(receipts)?;

    // Get the allocation id from the first receipt, return error if there are no receipts
    let (collection_id, payer, data_service, service_provider) = match receipts.first() {
        Some(receipt) => (
            receipt.message.collection_id,
            receipt.message.payer,
            receipt.message.data_service,
            receipt.message.service_provider,
        ),
        None => return Err(graph_tally_core::Error::NoValidReceiptsForRavRequest.into()),
    };

    // The payer is read before any signature is checked because it decides both halves of
    // the check: which key signs the RAV, and which signers are acceptable on the way in.
    // `check_collection_id` below proves the remaining receipts carry this same payer.
    let (wallet, accepted_addresses) = signers.resolve(payer).ok_or_else(|| {
        anyhow!(
            "no signing key configured for payer {payer}; \
             signing with another payer's key would produce an uncollectable RAV"
        )
    })?;

    // Check that the receipts are signed by a signer accepted for *this payer*. A signer
    // accepted for some other payer is not interchangeable: the RAV carries this payer, and
    // the collector requires its signer to be authorized for it.
    receipts.par_iter().try_for_each(|receipt| {
        check_signature_is_from_one_of_addresses(receipt, domain_separator, accepted_addresses)
    })?;

    // Check that the previous rav is signed by an accepted signer address
    if let Some(previous_rav) = &previous_rav {
        check_signature_is_from_one_of_addresses(
            previous_rav,
            domain_separator,
            accepted_addresses,
        )?;
    }

    // Check that the receipts timestamp is greater than the previous rav
    check_receipt_timestamps(receipts, previous_rav.as_ref())?;

    // Check that the receipts all have the same collection id
    check_collection_id(
        receipts,
        collection_id,
        payer,
        data_service,
        service_provider,
    )?;

    // Check that the rav has the correct collection id
    if let Some(previous_rav) = &previous_rav {
        let prev_id = previous_rav.message.collectionId;
        let prev_payer = previous_rav.message.payer;
        let prev_data_service = previous_rav.message.dataService;
        let prev_service_provider = previous_rav.message.serviceProvider;
        if prev_id != collection_id {
            return Err(graph_tally_core::Error::RavCollectionIdMismatch {
                prev_id: format!("{prev_id:#X}"),
                new_id: format!("{collection_id:#X}"),
            }
            .into());
        }
        if prev_payer != payer {
            return Err(graph_tally_core::Error::RavCollectionIdMismatch {
                prev_id: format!("{prev_id:#X}"),
                new_id: format!("{collection_id:#X}"),
            }
            .into());
        }

        if prev_data_service != data_service {
            return Err(graph_tally_core::Error::RavCollectionIdMismatch {
                prev_id: format!("{prev_id:#X}"),
                new_id: format!("{collection_id:#X}"),
            }
            .into());
        }
        if prev_service_provider != service_provider {
            return Err(graph_tally_core::Error::RavCollectionIdMismatch {
                prev_id: format!("{prev_id:#X}"),
                new_id: format!("{collection_id:#X}"),
            }
            .into());
        }
    }

    // Aggregate the receipts
    let rav = ReceiptAggregateVoucher::aggregate_receipts(
        collection_id,
        payer,
        data_service,
        service_provider,
        receipts,
        previous_rav,
    )?;

    // Sign the rav and return
    Ok(Eip712SignedMessage::new(domain_separator, rav, wallet)?)
}

fn check_signature_is_from_one_of_addresses<M: SolStruct>(
    message: &Eip712SignedMessage<M>,
    domain_separator: &Eip712Domain,
    accepted_addresses: &HashSet<Address>,
) -> Result<()> {
    let recovered_address = message.recover_signer(domain_separator)?;
    if !accepted_addresses.contains(&recovered_address) {
        bail!(graph_tally_core::Error::InvalidRecoveredSigner {
            address: recovered_address,
        });
    }
    Ok(())
}

fn check_collection_id(
    receipts: &[Eip712SignedMessage<Receipt>],
    collection_id: FixedBytes<32>,
    payer: Address,
    data_service: Address,
    service_provider: Address,
) -> Result<()> {
    for receipt in receipts.iter() {
        let receipt = &receipt.message;
        if receipt.collection_id != collection_id {
            return Err(graph_tally_core::Error::RavCollectionIdNotUniform.into());
        }
        if receipt.payer != payer {
            return Err(graph_tally_core::Error::RavCollectionIdNotUniform.into());
        }
        if receipt.data_service != data_service {
            return Err(graph_tally_core::Error::RavCollectionIdNotUniform.into());
        }
        if receipt.service_provider != service_provider {
            return Err(graph_tally_core::Error::RavCollectionIdNotUniform.into());
        }
    }
    Ok(())
}

fn check_signatures_unique(receipts: &[Eip712SignedMessage<Receipt>]) -> Result<()> {
    let mut receipt_signatures = HashSet::new();
    for receipt in receipts.iter() {
        let signature = receipt.unique_id();
        if !receipt_signatures.insert(signature) {
            return Err(graph_tally_core::Error::DuplicateReceiptSignature(format!(
                "{:?}",
                receipt.unique_id()
            ))
            .into());
        }
    }
    Ok(())
}

fn check_receipt_timestamps(
    receipts: &[Eip712SignedMessage<Receipt>],
    previous_rav: Option<&Eip712SignedMessage<ReceiptAggregateVoucher>>,
) -> Result<()> {
    if let Some(previous_rav) = &previous_rav {
        for receipt in receipts.iter() {
            let receipt = &receipt.message;
            if previous_rav.message.timestampNs >= receipt.timestamp_ns {
                return Err(graph_tally_core::Error::ReceiptTimestampLowerThanRav {
                    rav_ts: previous_rav.message.timestampNs,
                    receipt_ts: receipt.timestamp_ns,
                }
                .into());
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use graph_tally_core::{graph_tally_eip712_domain, signed_message::Eip712SignedMessage};
    use graph_tally_graph::{Receipt, ReceiptAggregateVoucher};
    use rstest::*;
    use thegraph_core::alloy::{
        dyn_abi::Eip712Domain,
        primitives::{address, fixed_bytes, Address, Bytes, FixedBytes},
        signers::local::PrivateKeySigner,
    };

    use crate::signers::{PayerKeys, SignerRegistry};

    #[fixture]
    fn keys() -> (PrivateKeySigner, Address) {
        let wallet = PrivateKeySigner::random();
        let address = wallet.address();
        (wallet, address)
    }

    #[fixture]
    fn collection_id() -> FixedBytes<32> {
        fixed_bytes!("deaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddead")
    }

    #[fixture]
    fn payer() -> Address {
        address!("abababababababababababababababababababab")
    }

    #[fixture]
    fn data_service() -> Address {
        address!("deaddeaddeaddeaddeaddeaddeaddeaddeaddead")
    }

    #[fixture]
    fn service_provider() -> Address {
        address!("beefbeefbeefbeefbeefbeefbeefbeefbeefbeef")
    }

    #[fixture]
    fn other_collection_id() -> FixedBytes<32> {
        fixed_bytes!("1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef")
    }
    #[fixture]
    fn domain_separator() -> Eip712Domain {
        graph_tally_eip712_domain(1, Address::from([0x11u8; 20]))
    }

    /// Two payers, each with its own signing key -- the shape that a payer migration needs.
    fn two_payer_registry(
        payer_a: Address,
        signer_a: &PrivateKeySigner,
        payer_b: Address,
        signer_b: &PrivateKeySigner,
    ) -> SignerRegistry {
        SignerRegistry::new(HashMap::from([
            (payer_a, PayerKeys::new(signer_a.clone(), [])),
            (payer_b, PayerKeys::new(signer_b.clone(), [])),
        ]))
    }

    fn receipt_for(
        domain_separator: &Eip712Domain,
        payer: Address,
        signer: &PrivateKeySigner,
        value: u128,
    ) -> Eip712SignedMessage<Receipt> {
        Eip712SignedMessage::new(
            domain_separator,
            Receipt::new(
                collection_id(),
                payer,
                data_service(),
                service_provider(),
                value,
            )
            .unwrap(),
            signer,
        )
        .unwrap()
    }

    #[rstest]
    #[test]
    /// The RAV must be signed by the key belonging to the payer named in the receipts.
    ///
    /// The collector recovers the RAV signer and requires it to be authorized for the RAV's
    /// payer, so signing payer A's receipts with payer B's key yields a RAV that is rejected
    /// by the indexer and uncollectable on chain.
    fn signs_each_payer_with_its_own_key(domain_separator: Eip712Domain) {
        let (payer_a, payer_b) = (Address::repeat_byte(0xa1), Address::repeat_byte(0xb2));
        let (signer_a, signer_b) = (PrivateKeySigner::random(), PrivateKeySigner::random());
        let registry = two_payer_registry(payer_a, &signer_a, payer_b, &signer_b);

        for (payer, expected) in [(payer_a, &signer_a), (payer_b, &signer_b)] {
            let receipts = vec![receipt_for(&domain_separator, payer, expected, 42)];
            let rav =
                super::check_and_aggregate_receipts(&domain_separator, &receipts, None, &registry)
                    .unwrap();
            assert_eq!(rav.message.payer, payer);
            assert_eq!(
                rav.recover_signer(&domain_separator).unwrap(),
                expected.address(),
            );
        }
    }

    #[rstest]
    #[test]
    /// A payer with no configured key is refused rather than signed with whatever key is at
    /// hand. Refusing is a loud failure; signing anyway is a silent one discovered days later.
    fn refuses_a_payer_it_holds_no_key_for(domain_separator: Eip712Domain) {
        let (payer_a, payer_b) = (Address::repeat_byte(0xa1), Address::repeat_byte(0xb2));
        let (signer_a, signer_b) = (PrivateKeySigner::random(), PrivateKeySigner::random());
        let registry = two_payer_registry(payer_a, &signer_a, payer_b, &signer_b);

        let unknown = Address::repeat_byte(0xcc);
        let receipts = vec![receipt_for(&domain_separator, unknown, &signer_a, 42)];
        let err =
            super::check_and_aggregate_receipts(&domain_separator, &receipts, None, &registry)
                .unwrap_err();
        assert!(err.to_string().contains("no signing key configured"));
    }

    #[rstest]
    #[test]
    /// Accepted signers are scoped per payer, so one payer's signer cannot vouch for another's
    /// receipts. This is the case that turns a payer migration into uncollectable RAVs: the
    /// receipts are accepted, the RAV carries the old payer, and the new key signs it.
    fn rejects_a_signer_belonging_to_a_different_payer(domain_separator: Eip712Domain) {
        let (payer_a, payer_b) = (Address::repeat_byte(0xa1), Address::repeat_byte(0xb2));
        let (signer_a, signer_b) = (PrivateKeySigner::random(), PrivateKeySigner::random());
        let registry = two_payer_registry(payer_a, &signer_a, payer_b, &signer_b);

        // Receipts claiming payer A, signed by payer B's signer.
        let receipts = vec![receipt_for(&domain_separator, payer_a, &signer_b, 42)];
        let err =
            super::check_and_aggregate_receipts(&domain_separator, &receipts, None, &registry)
                .unwrap_err();
        assert!(err.to_string().contains(&signer_b.address().to_string()));
    }

    #[rstest]
    #[test]
    /// A rotation *within* one payer is still supported: receipts from the payer's previous
    /// signer aggregate into a RAV signed by its current one. Both keys are authorized for
    /// that same payer on chain, so the result is valid.
    fn accepts_a_previous_signer_of_the_same_payer(domain_separator: Eip712Domain) {
        let payer = Address::repeat_byte(0xa1);
        let (old_signer, new_signer) = (PrivateKeySigner::random(), PrivateKeySigner::random());
        let registry = SignerRegistry::new(HashMap::from([(
            payer,
            PayerKeys::new(new_signer.clone(), [old_signer.address()]),
        )]));

        let receipts = vec![receipt_for(&domain_separator, payer, &old_signer, 42)];
        let rav =
            super::check_and_aggregate_receipts(&domain_separator, &receipts, None, &registry)
                .unwrap();
        assert_eq!(rav.message.payer, payer);
        assert_eq!(
            rav.recover_signer(&domain_separator).unwrap(),
            new_signer.address(),
        );
    }

    #[rstest]
    #[test]
    fn check_signatures_unique_fail(
        keys: (PrivateKeySigner, Address),
        collection_id: FixedBytes<32>,
        payer: Address,
        data_service: Address,
        service_provider: Address,
        domain_separator: Eip712Domain,
    ) {
        // Create the same receipt twice (replay attack)
        let mut receipts = Vec::new();
        let receipt = Eip712SignedMessage::new(
            &domain_separator,
            Receipt::new(collection_id, payer, data_service, service_provider, 42).unwrap(),
            &keys.0,
        )
        .unwrap();
        receipts.push(receipt.clone());
        receipts.push(receipt);

        let res = super::check_signatures_unique(&receipts);
        assert!(res.is_err());
    }

    #[rstest]
    #[test]
    fn check_signatures_unique_ok(
        keys: (PrivateKeySigner, Address),
        collection_id: FixedBytes<32>,
        payer: Address,
        data_service: Address,
        service_provider: Address,
        domain_separator: Eip712Domain,
    ) {
        // Create 2 different receipts
        let receipts = vec![
            Eip712SignedMessage::new(
                &domain_separator,
                Receipt::new(collection_id, payer, data_service, service_provider, 42).unwrap(),
                &keys.0,
            )
            .unwrap(),
            Eip712SignedMessage::new(
                &domain_separator,
                Receipt::new(collection_id, payer, data_service, service_provider, 42).unwrap(),
                &keys.0,
            )
            .unwrap(),
        ];

        let res = super::check_signatures_unique(&receipts);
        assert!(res.is_ok());
    }

    #[rstest]
    #[test]
    /// Test that a receipt with a timestamp greater than the rav timestamp passes
    fn check_receipt_timestamps(
        keys: (PrivateKeySigner, Address),
        collection_id: FixedBytes<32>,
        payer: Address,
        data_service: Address,
        service_provider: Address,
        domain_separator: Eip712Domain,
    ) {
        // Create receipts with consecutive timestamps
        let receipt_timestamp_range = 10..20;
        let mut receipts = Vec::new();
        for i in receipt_timestamp_range.clone() {
            receipts.push(
                Eip712SignedMessage::new(
                    &domain_separator,
                    Receipt {
                        collection_id,
                        payer,
                        data_service,
                        service_provider,
                        timestamp_ns: i,
                        nonce: 0,
                        value: 42,
                    },
                    &keys.0,
                )
                .unwrap(),
            );
        }

        // Create rav with max_timestamp below the receipts timestamps
        let rav = Eip712SignedMessage::new(
            &domain_separator,
            ReceiptAggregateVoucher {
                collectionId: collection_id,
                dataService: data_service,
                payer,
                serviceProvider: service_provider,
                timestampNs: receipt_timestamp_range.clone().min().unwrap() - 1,
                valueAggregate: 42,
                metadata: Bytes::new(),
            },
            &keys.0,
        )
        .unwrap();
        assert!(super::check_receipt_timestamps(&receipts, Some(&rav)).is_ok());

        // Create rav with max_timestamp equal to the lowest receipt timestamp
        // Aggregation should fail
        let rav = Eip712SignedMessage::new(
            &domain_separator,
            ReceiptAggregateVoucher {
                collectionId: collection_id,
                dataService: data_service,
                payer,
                serviceProvider: service_provider,
                timestampNs: receipt_timestamp_range.clone().min().unwrap(),
                valueAggregate: 42,
                metadata: Bytes::new(),
            },
            &keys.0,
        )
        .unwrap();
        assert!(super::check_receipt_timestamps(&receipts, Some(&rav)).is_err());

        // Create rav with max_timestamp above highest receipt timestamp
        // Aggregation should fail
        let rav = Eip712SignedMessage::new(
            &domain_separator,
            ReceiptAggregateVoucher {
                collectionId: collection_id,
                dataService: data_service,
                payer,
                serviceProvider: service_provider,
                timestampNs: receipt_timestamp_range.clone().max().unwrap() + 1,
                valueAggregate: 42,
                metadata: Bytes::new(),
            },
            &keys.0,
        )
        .unwrap();
        assert!(super::check_receipt_timestamps(&receipts, Some(&rav)).is_err());
    }

    #[rstest]
    #[test]
    /// Test check_allocation_id with 2 receipts that have the correct allocation id
    /// and 1 receipt that has the wrong allocation id
    fn check_allocation_id_fail(
        keys: (PrivateKeySigner, Address),
        collection_id: FixedBytes<32>,
        payer: Address,
        data_service: Address,
        service_provider: Address,
        other_collection_id: FixedBytes<32>,
        domain_separator: Eip712Domain,
    ) {
        let receipts = vec![
            Eip712SignedMessage::new(
                &domain_separator,
                Receipt::new(collection_id, payer, data_service, service_provider, 42).unwrap(),
                &keys.0,
            )
            .unwrap(),
            Eip712SignedMessage::new(
                &domain_separator,
                Receipt::new(collection_id, payer, data_service, service_provider, 43).unwrap(),
                &keys.0,
            )
            .unwrap(),
            Eip712SignedMessage::new(
                &domain_separator,
                Receipt::new(
                    other_collection_id,
                    payer,
                    data_service,
                    service_provider,
                    44,
                )
                .unwrap(),
                &keys.0,
            )
            .unwrap(),
        ];

        let res = super::check_collection_id(
            &receipts,
            collection_id,
            payer,
            data_service,
            service_provider,
        );

        assert!(res.is_err());
    }

    #[rstest]
    #[test]
    /// Test check_allocation_id with 3 receipts that have the correct allocation id
    fn check_allocation_id_ok(
        keys: (PrivateKeySigner, Address),
        collection_id: FixedBytes<32>,
        payer: Address,
        data_service: Address,
        service_provider: Address,
        domain_separator: Eip712Domain,
    ) {
        let receipts = vec![
            Eip712SignedMessage::new(
                &domain_separator,
                Receipt::new(collection_id, payer, data_service, service_provider, 42).unwrap(),
                &keys.0,
            )
            .unwrap(),
            Eip712SignedMessage::new(
                &domain_separator,
                Receipt::new(collection_id, payer, data_service, service_provider, 43).unwrap(),
                &keys.0,
            )
            .unwrap(),
            Eip712SignedMessage::new(
                &domain_separator,
                Receipt::new(collection_id, payer, data_service, service_provider, 44).unwrap(),
                &keys.0,
            )
            .unwrap(),
        ];

        let res = super::check_collection_id(
            &receipts,
            collection_id,
            payer,
            data_service,
            service_provider,
        );

        assert!(res.is_ok());
    }
}
