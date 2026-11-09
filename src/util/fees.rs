use crate::chain::{Network, OutPoint, Transaction, TxOut};
use std::collections::HashMap;

use electrs_macros::trace;

const VSIZE_BIN_WIDTH: u64 = 50_000; // in vbytes

pub struct TxFeeInfo {
    pub fee: u64,           // in satoshis
    pub vsize: u64,         // in virtual bytes (= weight/4)
    pub fee_per_vbyte: f64, // in sat/vb
}

impl TxFeeInfo {
    pub fn new(
        tx: &Transaction,
        prevouts: &HashMap<u32, &TxOut>,
        refheights: &HashMap<OutPoint, u32>,
        network: Network,
    ) -> Self {
        let fee = get_tx_fee(tx, prevouts, refheights, network);

        let weight = tx.weight();
        #[cfg(not(feature = "liquid"))] // rust-bitcoin has a wrapper Weight type
        let weight = weight.to_wu();

        let vsize_float = weight as f64 / 4f64; // for more accurate sat/vB

        TxFeeInfo {
            fee,
            vsize: vsize_float.ceil() as u64,
            fee_per_vbyte: fee as f64 / vsize_float,
        }
    }
}

/// Freicoin: the inputs are valued at the transaction's `lock_height` (demurrage), each from its
/// reference height in `refheights`; an input whose reference height is unknown counts at its face
/// value, which overstates the fee.
#[cfg(not(feature = "liquid"))]
pub fn get_tx_fee(
    tx: &Transaction,
    prevouts: &HashMap<u32, &TxOut>,
    refheights: &HashMap<OutPoint, u32>,
    _network: Network,
) -> u64 {
    crate::freicoin::tx_fee(
        tx,
        prevouts.iter().map(|(&index, &txo)| {
            let outpoint = tx.input[index as usize].previous_output;
            (txo, refheights.get(&outpoint).copied().unwrap_or(tx.lock_height))
        }),
    )
}

#[cfg(feature = "liquid")]
pub fn get_tx_fee(
    tx: &Transaction,
    _prevouts: &HashMap<u32, &TxOut>,
    _refheights: &HashMap<OutPoint, u32>,
    network: Network,
) -> u64 {
    tx.fee_in(*network.native_asset())
}

#[trace]
pub fn make_fee_histogram(mut entries: Vec<&TxFeeInfo>) -> Vec<(f64, u64)> {
    entries.sort_unstable_by(|e1, e2| e1.fee_per_vbyte.partial_cmp(&e2.fee_per_vbyte).unwrap());

    let mut histogram = vec![];
    let mut bin_size = 0;
    let mut last_fee_rate = 0.0;
    for e in entries.iter().rev() {
        if bin_size > VSIZE_BIN_WIDTH && last_fee_rate != e.fee_per_vbyte {
            // vsize of transactions paying >= last_fee_rate
            histogram.push((last_fee_rate, bin_size));
            bin_size = 0;
        }
        last_fee_rate = e.fee_per_vbyte;
        bin_size += e.vsize;
    }
    if bin_size > 0 {
        histogram.push((last_fee_rate, bin_size));
    }
    histogram
}
