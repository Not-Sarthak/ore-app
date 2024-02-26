use std::rc::Rc;

use dioxus::prelude::UseState;
use ore::EPOCH_DURATION;
use serde::{Deserialize, Serialize};
#[cfg(feature = "web")]
use serde_wasm_bindgen::to_value;
#[cfg(feature = "web")]
use solana_client_wasm::solana_sdk::{
    keccak::{hashv, Hash as KeccakHash},
    pubkey::Pubkey,
    signature::Signature,
    signer::Signer,
};
#[cfg(feature = "desktop")]
use solana_sdk::{
    keccak::{hashv, Hash as KeccakHash},
    pubkey::Pubkey,
    signature::Signature,
    signer::Signer,
};
#[cfg(feature = "web")]
use web_sys::Worker;

use super::{signer, Gateway, GatewayResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WebworkerRequest {
    Pause,
    Mine(MineRequest),
}

/// Mining request for web workers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MineRequest {
    pub hash: KeccakHash,
    pub difficulty: KeccakHash,
    pub pubkey: Pubkey,
}

/// Mining response from web workers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebworkerResponse {
    pub hash: KeccakHash,
    pub nonce: u64,
}

/// Finds the a valid hash given the mining request.
pub async fn find_next_hash(req: MineRequest) -> Option<WebworkerResponse> {
    let mut next_hash: KeccakHash;
    let mut nonce = 0u64;
    loop {
        if nonce % 10_000 == 0 {
            log::info!("Nonce: {}", nonce);
        }
        next_hash = hashv(&[
            req.hash.to_bytes().as_slice(),
            req.pubkey.to_bytes().as_slice(),
            nonce.to_be_bytes().as_slice(),
        ]);
        if next_hash.le(&req.difficulty) {
            break;
        }
        nonce += 1;
    }
    Some(WebworkerResponse {
        hash: next_hash,
        nonce,
    })
}

pub async fn mine(gateway: &Rc<Gateway>, worker: &UseState<Worker>) -> GatewayResult<()> {
    let signer = signer();
    let treasury = gateway.get_treasury().await?;
    let proof = gateway.get_proof(signer.pubkey()).await?;
    let req = WebworkerRequest::Mine(MineRequest {
        hash: proof.hash.into(),
        difficulty: treasury.difficulty.into(),
        pubkey: signer.pubkey(),
    });
    let msg = to_value(&req).unwrap();
    worker.get().post_message(&msg).unwrap();
    Ok(())
}

pub async fn submit_solution(
    gateway: &Rc<Gateway>,
    res: &WebworkerResponse,
) -> GatewayResult<Signature> {
    // Submit mine tx.
    let mut bus_id = 0;
    let next_hash = res.hash;
    let nonce = res.nonce;
    let signer = signer();
    loop {
        // Check if epoch needs to be reset
        let treasury = gateway.get_treasury().await?;
        let clock = gateway.get_clock().await?;
        let epoch_end_at = treasury.epoch_start_at.saturating_add(EPOCH_DURATION);

        // Submit restart epoch tx, if needed.
        if clock.unix_timestamp.ge(&epoch_end_at) {
            let ix = ore::instruction::reset(signer.pubkey());
            gateway.send_and_confirm(&[ix]).await?;
        }

        // Submit mine tx
        let ix = ore::instruction::mine(
            signer.pubkey(),
            ore::BUS_ADDRESSES[bus_id],
            next_hash.into(),
            nonce,
        );
        match gateway.send_and_confirm(&[ix]).await {
            Ok(sig) => return Ok(sig),
            Err(_err) => {
                // Retry on different bus.
                bus_id += 1;
                if bus_id.ge(&ore::BUS_COUNT) {
                    bus_id = 0;
                }
            }
        }
    }
}
