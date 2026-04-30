//! # EIP712 message and signatures
//!
//! This module contains the `EIP712SignedMessage` struct which is used to sign and verify messages
//! using EIP712 standard.
//!
//! # Example
//! ```rust
//! # use thegraph_core::alloy::{dyn_abi::Eip712Domain, primitives::{Address, FixedBytes}, signers::local::PrivateKeySigner};
//! # use graph_tally_graph::{Receipt};
//! # let domain_separator = Eip712Domain::default();
//! use graph_tally_core::signed_message::Eip712SignedMessage;
//! # let wallet = PrivateKeySigner::random();
//! # let wallet_address = wallet.address();
//! # let collection_id = FixedBytes::from([0xab; 32]);
//! # let payer = Address::from([0x11u8; 20]);
//! # let data_service = Address::from([0x22u8; 20]);
//! # let service_provider = Address::from([0x33u8; 20]);
//! # let message = Receipt::new(collection_id, payer, data_service, service_provider, 100).unwrap();
//!
//! let signed_message = Eip712SignedMessage::new(&domain_separator, message, &wallet).unwrap();
//! let signer = signed_message.recover_signer(&domain_separator).unwrap();
//!
//! assert_eq!(signer, wallet_address);
//! ```
//!

pub use ::graph_tally_eip712_message::*;
