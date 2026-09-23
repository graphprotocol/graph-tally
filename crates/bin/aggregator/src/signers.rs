//! Per-payer signing keys.
//!
//! A GraphTally payer can accept a number of signers to manage receipts/RAVs on its behalf.
//! These signers must be authorized on chain for that specific payer.
//!
//! The aggregator process will accept receipts and RAVs from any of those signers, process
//! them and sign with a configured signer for the target payer.

use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, bail};
use thegraph_core::alloy::{primitives::Address, signers::local::PrivateKeySigner};

/// For any given GraphTally payer, one key signs but several signers may be accepted
/// - `signing_key`, used to sign RAVs for this payer
/// - `accepted_signers`, list of signer addresses for which to accept receipts/RAVs for this payer
pub struct PayerKeys {
    signing_key: PrivateKeySigner,
    accepted_signers: HashSet<Address>,
}

impl PayerKeys {
    /// Keys for one payer, accepting `also_accept` on top of `signing_key`'s own address.
    pub fn new(
        signing_key: PrivateKeySigner,
        also_accept: impl IntoIterator<Item = Address>,
    ) -> Self {
        let mut accepted_signers = HashSet::from([signing_key.address()]);
        accepted_signers.extend(also_accept);
        Self {
            signing_key,
            accepted_signers,
        }
    }
}

/// Resolves a payer to the key that may sign RAVs on its behalf.
pub struct SignerRegistry(HashMap<Address, PayerKeys>);

impl SignerRegistry {
    pub fn new(payers: HashMap<Address, PayerKeys>) -> Self {
        Self(payers)
    }

    /// The key that may sign for `payer`, and the signers whose receipts are accepted for it.
    ///
    /// `None` means no key is held for that payer. Signing anyway would produce a RAV that
    /// neither the indexer nor the collector accepts, so refusing is the only safe answer.
    pub fn resolve(&self, payer: Address) -> Option<(&PrivateKeySigner, &HashSet<Address>)> {
        self.0
            .get(&payer)
            .map(|keys| (&keys.signing_key, &keys.accepted_signers))
    }

    /// `(payer, signing address, accepted signers)` per entry, for logging at startup.
    pub fn summary(&self) -> Vec<(Address, Address, Vec<Address>)> {
        let mut out: Vec<_> = self
            .0
            .iter()
            .map(|(payer, keys)| {
                let mut accepted: Vec<Address> = keys.accepted_signers.iter().copied().collect();
                accepted.sort();
                (*payer, keys.signing_key.address(), accepted)
            })
            .collect();
        out.sort_by_key(|(payer, _, _)| *payer);
        out
    }
}

/// Build a registry from already-parsed `(payer, signing key)` and `(payer, accepted signer)`
/// pairs. A signer is bound to exactly one authorizer on chain, so an address may appear under
/// one payer only, whichever way it arrives.
pub fn build(
    signing_keys: impl IntoIterator<Item = (Address, PrivateKeySigner)>,
    accepted: impl IntoIterator<Item = (Address, Address)>,
) -> anyhow::Result<SignerRegistry> {
    let mut payers: HashMap<Address, PayerKeys> = HashMap::new();
    // Every signer address seen so far and the payer it belongs to
    let mut signer_payer: HashMap<Address, Address> = HashMap::new();
    for (payer, signing_key) in signing_keys {
        if let Some(other) = signer_payer.insert(signing_key.address(), payer) {
            bail!(
                "signing key {} is listed for both {other} and {payer}, but a signer can only \
                 be authorized for one payer",
                signing_key.address(),
            );
        }
        if payers
            .insert(payer, PayerKeys::new(signing_key, []))
            .is_some()
        {
            bail!("duplicate entry for payer {payer}");
        }
    }
    if payers.is_empty() {
        bail!("no payers configured");
    }

    for (payer, accepted) in accepted {
        let keys = payers.get_mut(&payer).ok_or_else(|| {
            anyhow!("accepted signer {accepted} names payer {payer}, which has no signing key")
        })?;
        if let Some(other) = signer_payer.insert(accepted, payer) {
            if other != payer {
                bail!(
                    "signer {accepted} is listed for both {other} and {payer}, but a signer can \
                     only be authorized for one payer"
                );
            }
        }
        keys.accepted_signers.insert(accepted);
    }

    Ok(SignerRegistry::new(payers))
}

#[cfg(test)]
mod tests {
    use thegraph_core::alloy::{primitives::Address, signers::local::PrivateKeySigner};

    use super::build;

    fn payer(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn key(n: u8) -> PrivateKeySigner {
        PrivateKeySigner::from_bytes(&Address::repeat_byte(n).into_word()).unwrap()
    }

    #[test]
    fn selects_the_key_for_the_payer() {
        let (a, b) = (key(1), key(2));
        let registry = build([(payer(1), a.clone()), (payer(2), b.clone())], []).unwrap();
        assert_eq!(registry.resolve(payer(1)).unwrap().0.address(), a.address());
        assert_eq!(registry.resolve(payer(2)).unwrap().0.address(), b.address());
    }

    #[test]
    fn unknown_payer_is_refused() {
        let registry = build([(payer(1), key(1))], []).unwrap();
        // Signing here would produce a RAV no indexer or collector would accept.
        assert!(registry.resolve(payer(3)).is_none());
    }

    #[test]
    fn a_payers_own_signer_is_accepted_without_being_listed() {
        let a = key(1);
        let registry = build([(payer(1), a.clone())], []).unwrap();
        // Needed for `previous_rav`: the RAV this process signs comes back as input, and is
        // checked against this same set.
        assert!(registry.resolve(payer(1)).unwrap().1.contains(&a.address()));
    }

    #[test]
    fn accepted_signers_are_scoped_to_their_payer() {
        let other = payer(7);
        let registry = build(
            [(payer(1), key(1)), (payer(2), key(2))],
            [(payer(1), other)],
        )
        .unwrap();
        // A signer accepted for one payer must not be accepted for the other -- accepting it
        // across payers is what turns a migration into uncollectable RAVs.
        assert!(registry.resolve(payer(1)).unwrap().1.contains(&other));
        assert!(!registry.resolve(payer(2)).unwrap().1.contains(&other));
    }

    #[test]
    fn accepted_signers_accumulate_for_one_payer() {
        let registry = build(
            [(payer(1), key(1))],
            [(payer(1), payer(7)), (payer(1), payer(8))],
        )
        .unwrap();
        let (_, accepted) = registry.resolve(payer(1)).unwrap();
        assert_eq!(accepted.len(), 3); // own signer + two listed
    }

    #[test]
    fn an_accepted_signer_cannot_be_shared_across_payers() {
        let shared = payer(9);
        // Accepting one address for two payers is a config the chain cannot honour, and it
        // would let receipts signed by a key authorized elsewhere draw on the other's escrow.
        assert!(build(
            [(payer(1), key(1)), (payer(2), key(2))],
            [(payer(1), shared), (payer(2), shared)],
        )
        .is_err());
        // Same, when the shared address is another payer's signing key.
        let a_key = key(1);
        assert!(build(
            [(payer(1), a_key.clone()), (payer(2), key(2))],
            [(payer(2), a_key.address())],
        )
        .is_err());
    }

    #[test]
    fn repeating_a_signer_for_its_own_payer_is_allowed() {
        let extra = payer(9);
        // Listing the same pair twice is idempotent, not a conflict.
        assert!(build([(payer(1), key(1))], [(payer(1), extra), (payer(1), extra)]).is_ok());
        // Nor is redundantly listing a payer's own signing key, which is accepted anyway.
        let a_key = key(1);
        assert!(build([(payer(1), a_key.clone())], [(payer(1), a_key.address())]).is_ok());
    }

    #[test]
    fn rejects_configs_the_chain_could_not_honour() {
        // One key cannot be authorized for two payers.
        assert!(build([(payer(1), key(1)), (payer(2), key(1))], []).is_err());
        // Two keys for one payer: only one can sign, so the second is silently lost.
        assert!(build([(payer(1), key(1)), (payer(1), key(2))], []).is_err());
        // Accepted signer naming a payer with no signing key.
        assert!(build([(payer(1), key(1))], [(payer(5), payer(7))]).is_err());
        // Nothing configured.
        assert!(build([], []).is_err());
    }

    #[test]
    fn summary_reports_every_payer() {
        let a = key(1);
        let summary = build([(payer(1), a.clone()), (payer(2), key(2))], [])
            .unwrap()
            .summary();
        assert_eq!(summary.len(), 2);
        assert_eq!(summary[0].0, payer(1));
        assert_eq!(summary[0].1, a.address());
    }
}
