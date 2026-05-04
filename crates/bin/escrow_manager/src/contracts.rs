use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy::{
    network::EthereumWallet,
    primitives::{keccak256, Address, BlockNumber, Bytes, U256},
    providers::{DynProvider, Provider as _, ProviderBuilder, WalletProvider},
    signers::{local::PrivateKeySigner, SignerSync as _},
    sol,
    sol_types::SolInterface,
};
use anyhow::{anyhow, Context as _};
use reqwest::Url;

sol!(
    #[allow(missing_docs)]
    #[sol(rpc)]
    ERC20,
    "src/abi/ERC20.abi.json"
);
use ERC20::ERC20Instance;
sol!(
    #[allow(missing_docs)]
    #[sol(rpc)]
    #[derive(Debug)]
    PaymentsEscrow,
    "src/abi/PaymentsEscrow.abi.json"
);
use PaymentsEscrow::{PaymentsEscrowErrors, PaymentsEscrowInstance};
sol!(
    #[allow(missing_docs)]
    #[sol(rpc)]
    #[derive(Debug)]
    GraphTallyCollector,
    "src/abi/GraphTallyCollector.abi.json"
);
use GraphTallyCollector::{GraphTallyCollectorErrors, GraphTallyCollectorInstance};

pub struct Contracts {
    payments_escrow: PaymentsEscrowInstance<DynProvider>,
    graph_tally_collector: GraphTallyCollectorInstance<DynProvider>,
    token: ERC20Instance<DynProvider>,
    payer: Address,
}

impl Contracts {
    pub fn new(
        payer: PrivateKeySigner,
        chain_rpc: Url,
        token: Address,
        payments_escrow: Address,
        graph_tally_collector: Address,
    ) -> Self {
        let provider = ProviderBuilder::new()
            .with_simple_nonce_management()
            .wallet(EthereumWallet::from(payer))
            .connect_http(chain_rpc);
        let payer = provider.default_signer_address();
        let provider = provider.erased();
        let payments_escrow = PaymentsEscrowInstance::new(payments_escrow, provider.clone());
        let graph_tally_collector =
            GraphTallyCollectorInstance::new(graph_tally_collector, provider.clone());
        let token = ERC20Instance::new(token, provider.clone());
        Self {
            payments_escrow,
            graph_tally_collector,
            token,
            payer,
        }
    }

    pub fn payer(&self) -> Address {
        self.payer
    }

    pub async fn allowance(&self) -> anyhow::Result<u128> {
        self.token
            .allowance(self.payer(), *self.payments_escrow.address())
            .call()
            .await
            .context("get allowance")?
            .try_into()
            .context("result out of bounds")
    }

    pub async fn approve(&self, amount: u128) -> anyhow::Result<()> {
        self.token
            .approve(*self.payments_escrow.address(), U256::from(amount))
            .send()
            .await?
            .with_timeout(Some(Duration::from_secs(30)))
            .with_required_confirmations(1)
            .watch()
            .await?;
        Ok(())
    }

    pub async fn deposit_many(
        &self,
        deposits: impl IntoIterator<Item = (Address, u128)>,
    ) -> anyhow::Result<BlockNumber> {
        // Create individual deposit calls for multicall
        let calls: Vec<Bytes> = deposits
            .into_iter()
            .map(|(receiver, amount)| {
                self.payments_escrow
                    .deposit(
                        *self.graph_tally_collector.address(),
                        receiver,
                        U256::from(amount),
                    )
                    .calldata()
                    .clone()
            })
            .collect();

        // Execute all deposits in a single multicall transaction
        let receipt = self
            .payments_escrow
            .multicall(calls)
            .send()
            .await
            .map_err(decoded_err::<PaymentsEscrowErrors>)?
            .with_timeout(Some(Duration::from_secs(30)))
            .with_required_confirmations(1)
            .get_receipt()
            .await?;

        let block_number = receipt
            .block_number
            .ok_or_else(|| anyhow!("invalid deposit receipt"))?;
        Ok(block_number)
    }

    pub async fn authorize_signer(&self, signer: &PrivateKeySigner) -> anyhow::Result<()> {
        let chain_id = self
            .graph_tally_collector
            .provider()
            .get_chain_id()
            .await
            .context("get chain ID")?;
        let deadline_offset_s = 60;
        let deadline = U256::from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + deadline_offset_s,
        );
        // Build the message according to the contract's expectation:
        // abi.encodePacked(block.chainid, address(this), "authorizeSignerProof", _proofDeadline, msg.sender)
        let mut message = Vec::new();
        message.extend_from_slice(&U256::from(chain_id).to_be_bytes::<32>());
        message.extend_from_slice(&self.graph_tally_collector.address().0 .0);
        message.extend_from_slice(b"authorizeSignerProof");
        message.extend_from_slice(&deadline.to_be_bytes::<32>());
        message.extend_from_slice(&self.payer().0 .0);

        let hash = keccak256(&message);

        // Sign with Ethereum message prefix (matching toEthSignedMessageHash)
        let signature = signer
            .sign_message_sync(hash.as_slice())
            .context("sign authorization proof")?;
        let proof: Bytes = signature.as_bytes().into();

        self.graph_tally_collector
            .authorizeSigner(signer.address(), deadline, proof)
            .send()
            .await
            .map_err(decoded_err::<GraphTallyCollectorErrors>)?
            .with_timeout(Some(Duration::from_secs(60)))
            .with_required_confirmations(1)
            .watch()
            .await?;
        Ok(())
    }
}

fn decoded_err<E: SolInterface + std::fmt::Debug>(err: alloy::contract::Error) -> anyhow::Error {
    match err {
        alloy::contract::Error::TransportError(alloy::transports::RpcError::ErrorResp(err)) => {
            match err.as_decoded_interface_error::<E>() {
                Some(decoded) => anyhow!("{:?}", decoded),
                None => anyhow!(err),
            }
        }
        _ => anyhow!(err),
    }
}
