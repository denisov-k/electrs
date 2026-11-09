//! Freicoin specifics that are not in the transaction and block formats: demurrage.
//!
//! An output's value is denominated at its transaction's reference height (`lock_height`) and
//! decays by a factor of (1 - 2^-20) per block after it.  A transaction's fee is therefore its
//! inputs' values adjusted to its own `lock_height`, less its outputs.

use crate::chain::{Network, Transaction, TxOut};

/// The exponentiation ladder of (1 - 2^-20)^(2^i), i = 0..26, as 64-bit fixed point fractions,
/// most significant word first (consensus/amount.cpp).
const K32: [u32; 52] = [
    0xfffff000, 0x00000000, // 2^0 = 1
    0xffffe000, 0x01000000, // 2^1 = 2
    0xffffc000, 0x05ffffc0, // 2^2 = 4
    0xffff8000, 0x1bfffc80, // 2^3 = 8
    0xffff0000, 0x77ffdd00, // ...
    0xfffe0001, 0xeffeca00,
    0xfffc0007, 0xdff5d409,
    0xfff8001f, 0xbfaca8a2,
    0xfff0007f, 0x7d5d5a6a,
    0xffe001fe, 0xeacb48a8,
    0xffc007fd, 0x55dfda2a,
    0xff801ff6, 0xad5499cd,
    0xff007fcd, 0x67f98aad,
    0xfe01fe9b, 0x74f0943e,
    0xfc07f540, 0x767d2a82,
    0xf81fab16, 0x3dc15990,
    0xf07d5f65, 0xf9604ac9,
    0xe1eb5045, 0x80b6ebf7,
    0xc75f7b66, 0xa5075def,
    0x9b459576, 0x663bbb3e,
    0x5e2d55e7, 0x48e27ab4,
    0x22a5531d, 0x29a95916,
    0x04b054d7, 0xfda49c4d,
    0x0015fc1b, 0x85085be9,
    0x000001e3, 0x54ca043c,
    0x00000000, 0x00039089,
];

/// Overflow-aware 64-bit multiply-and-accumulate of the C++.
struct Acc {
    sum: u64,
    overflow: u64,
}

impl Acc {
    fn new(sum: u64) -> Acc {
        Acc { sum, overflow: 0 }
    }
    fn term(&mut self, val: u64) {
        let sum = self.sum.wrapping_add(val);
        self.overflow += (sum < self.sum) as u64;
        self.sum = sum;
    }
    fn shift32(&mut self) {
        self.sum = (self.overflow << 32).wrapping_add(self.sum >> 32);
        self.overflow = 0;
    }
}

/// The value at `distance` blocks after its reference height of `value` at that height, exactly as
/// `TimeAdjustValueForward` in Freicoin's consensus/amount.cpp computes it.
pub fn time_adjust_value_forward(value: i64, distance: u32) -> i64 {
    if distance == 0 {
        return value;
    }
    // 2^26 blocks decay even MAX_MONEY to zero.
    if distance >= 1 << 26 {
        return 0;
    }
    let sign = value.signum();
    let value = value.unsigned_abs();

    // The demurrage factor for `distance`, the product of the ladder entries of its set bits, as
    // the first 64 fraction bits; the first entry is copied, the accumulator then being 1.0.
    let mut w: [u64; 2] = [0, 0];
    let mut first = true;
    let mut distance = distance;
    let mut bit = 0;
    while distance != 0 {
        if distance & 1 != 0 {
            let (k0, k1) = (K32[2 * bit] as u64, K32[2 * bit + 1] as u64);
            if first {
                first = false;
                w = [k0, k1];
            } else {
                let mut acc = Acc::new(k1 * w[0]);
                acc.term(k0 * w[1]);
                acc.shift32();
                acc.term(k0 * w[0]);
                w[1] = acc.sum & 0xffff_ffff;
                acc.shift32();
                w[0] = acc.sum & 0xffff_ffff;
            }
        }
        distance >>= 1;
        bit += 1;
    }

    // The factor times the value.
    let (v0, v1) = (value >> 32, value & 0xffff_ffff);
    let mut acc = Acc::new((w[1] * v1) >> 32);
    acc.term(w[1] * v0);
    acc.term(w[0] * v1);
    acc.shift32();
    acc.term(w[0] * v0);
    sign * acc.sum as i64
}

/// The value at `height` of `value` denominated at `refheight`.  Values are not adjusted
/// backwards: a transaction's `lock_height` is never below its inputs'.
pub fn value_at(value: u64, refheight: u32, height: u32) -> u64 {
    if height <= refheight {
        return value;
    }
    time_adjust_value_forward(value as i64, height - refheight) as u64
}

/// Whether an output script is the witness commitment of a block-final transaction: a single push
/// of a path byte, the 32-byte witness Merkle root and the magic "KJIH" (4b4a4948), possibly more
/// (consensus/validation.h, consensus/merkle.cpp).
pub fn is_witness_commitment(script: &[u8]) -> bool {
    match script.split_first() {
        Some((&len, data)) => {
            (37..=75).contains(&len)
                && data.len() == len as usize
                && data[33..37] == [0x4b, 0x4a, 0x49, 0x48]
        }
        None => false,
    }
}

/// Mainnet heights of the soft forks that each dropped a kria credited to every input on top of
/// its adjusted value: the truncation of fractional inputs, then the ALU demurrage arithmetic
/// (`truncate_inputs_activation_height` and `alu_activation_height` in kernel/chainparams.cpp).
const TRUNCATE_INPUTS_HEIGHT: usize = 158_425;
const ALU_HEIGHT: usize = 229_376;

/// What each input of a transaction confirmed at `height` counts for on top of its adjusted value,
/// as consensus (`per_input_adjustment` of `CheckTxInputs`) has it: a kria before each of the
/// truncation and ALU soft forks on mainnet, none on the test chains or in the mempool.
pub fn per_input_adjustment(network: Network, height: Option<usize>) -> u64 {
    match (network, height) {
        (Network::Bitcoin, Some(height)) => {
            (height < TRUNCATE_INPUTS_HEIGHT) as u64 + (height < ALU_HEIGHT) as u64
        }
        _ => 0,
    }
}

/// A transaction's fee: its inputs' values adjusted to its `lock_height`, each plus `adjustment`
/// (see [`per_input_adjustment`]), less its outputs.  `prevouts` gives each input's spent output
/// with the `lock_height` of the transaction that created it.  A coinbase pays no fee.
pub fn tx_fee<'a>(
    tx: &Transaction,
    prevouts: impl Iterator<Item = (&'a TxOut, u32)>,
    adjustment: u64,
) -> u64 {
    if tx.is_coinbase() {
        return 0;
    }
    let total_in: u64 = prevouts
        .map(|(txo, refheight)| {
            value_at(txo.value.to_sat(), refheight, tx.lock_height) + adjustment
        })
        .sum();
    let total_out: u64 = tx.output.iter().map(|out| out.value.to_sat()).sum();
    total_in.saturating_sub(total_out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::{deserialize, Block, BlockHeader};
    use bitcoin::hex::FromHex;

    /// (value, distance, TimeAdjustValueForward(value, distance)), from the C++ of Freicoin 28.1.
    const VECTORS: &[(i64, u32, i64)] = &[
        (0, 0, 0),
        (0, 1, 0),
        (0, 2, 0),
        (0, 3, 0),
        (0, 100, 0),
        (0, 1000, 0),
        (0, 12345, 0),
        (0, 100000, 0),
        (0, 412179, 0),
        (0, 1048576, 0),
        (0, 67108863, 0),
        (0, 67108864, 0),
        (1, 0, 1),
        (1, 1, 0),
        (1, 2, 0),
        (1, 3, 0),
        (1, 100, 0),
        (1, 1000, 0),
        (1, 12345, 0),
        (1, 100000, 0),
        (1, 412179, 0),
        (1, 1048576, 0),
        (1, 67108863, 0),
        (1, 67108864, 0),
        (100000000, 0, 100000000),
        (100000000, 1, 99999904),
        (100000000, 2, 99999809),
        (100000000, 3, 99999713),
        (100000000, 100, 99990463),
        (100000000, 1000, 99904677),
        (100000000, 12345, 98829591),
        (100000000, 100000, 90903882),
        (100000000, 412179, 67497156),
        (100000000, 1048576, 36787926),
        (100000000, 67108863, 0),
        (100000000, 67108864, 0),
        (5000000000, 0, 5000000000),
        (5000000000, 1, 4999995231),
        (5000000000, 2, 4999990463),
        (5000000000, 3, 4999985694),
        (5000000000, 100, 4999523185),
        (5000000000, 1000, 4995233899),
        (5000000000, 12345, 4941479584),
        (5000000000, 100000, 4545194110),
        (5000000000, 412179, 3374857829),
        (5000000000, 1048576, 1839396328),
        (5000000000, 67108863, 0),
        (5000000000, 67108864, 0),
        (9007199254740991, 0, 9007199254740991),
        (9007199254740991, 1, 9007190664806399),
        (9007199254740991, 2, 9007182074879999),
        (9007199254740991, 3, 9007173484961790),
        (9007199254740991, 100, 9006340301830927),
        (9007199254740991, 1000, 8998613410755119),
        (9007199254740991, 12345, 8901778246133741),
        (9007199254740991, 100000, 8187893801116753),
        (9007199254740991, 412179, 6079603385893163),
        (9007199254740991, 1048576, 3313561848323150),
        (9007199254740991, 67108863, 0),
        (9007199254740991, 67108864, 0),
        (3596603678300394, 37063010, 1),
        (5855293191497, 132877, 5158390584609),
        (2176882751481174, 296106, 1641327065946968),
        (3904101224083, 43984009, 0),
        (2047335983776742, 453430, 1328585279458746),
        (7145868741045, 498393, 4442554554210),
        (2228271332887298, 50118744, 0),
        (1605209380274, 425671, 1069618999211),
        (4215442372403666, 569255, 2449466892613926),
        (5826085035617, 3920503, 138547847040),
        (2067963811063178, 302885, 1549156631198861),
        (8263479328759, 195964, 6854873109745),
        (7467063353357582, 20244054, 30800288),
        (4731217883945, 199058, 3913163231406),
        (2465325934416616, 154781, 2127001600987339),
        (3653570760932, 30655737, 0),
        (2716333178950862, 476763, 1723929072088970),
        (1251351218009, 32093, 1213632171028),
        (3232540777605937, 23521453, 585504),
        (1900854574730, 74810, 1769963901238),
        (7599448354201519, 217321, 6176934450201011),
        (3060645872302, 9940805, 233654046),
        (1456792521989033, 198860, 1205132297352718),
        (9312956118615, 243668, 7381866630175),
        (5150104670548829, 62232299, 0),
        (3719666914329, 421631, 2488139587691),
        (7842562309232953, 357506, 5576833562385805),
        (4235676463461, 26778309, 34),
        (7361899985386998, 129319, 6507724018014794),
        (2027539518062, 552322, 1197322053454),
        (2173326418778574, 37719690, 0),
        (388796003067, 429195, 258202032180),
        (7511302669346199, 444963, 4913855763474284),
        (4745044716025, 26684618, 42),
        (2611676871573138, 225439, 2106435132156683),
        (2932051742830, 13882, 2893490382851),
        (826109676321426, 24127667, 83935),
        (7938224153045, 338141, 5750075879427),
        (8922106513683308, 163523, 7633789006999837),
        (7062449318435, 23448768, 1371),
    ];

    #[test]
    fn demurrage_matches_freicoin() {
        for &(value, distance, adjusted) in VECTORS {
            assert_eq!(time_adjust_value_forward(value, distance), adjusted, "{} {}", value, distance);
            assert_eq!(time_adjust_value_forward(-value, distance), -adjusted);
        }
    }

    struct BlockFixture {
        hash: &'static str,
        merkle_root: &'static str,
        txids: &'static [&'static str],
        header: &'static str,
        hex: &'static str,
    }

    struct TxFixture {
        txid: &'static str,
        wtxid: &'static str,
        size: usize,
        weight: u64,
        lock_height: u32,
        fee: u64,
        /// (value, refheight, value at the spending transaction's lock_height), from the node
        prevouts: &'static [(u64, u32, u64)],
        hex: &'static str,
    }

    // Mainnet, read from freicoind 28.1: block 1, and block 497291, merge-mined.
    const BLOCKS: &[BlockFixture] = &[
        BlockFixture {
            hash: "000000005d231b285e63af83edae2d8f5e50e70d396468643092b9239fd3be3c",
            merkle_root: "a0b9ec1d860e0786b0be7380eac76fe6180ca98b20aa03c08097bda1fbbce895",
            txids: &["a0b9ec1d860e0786b0be7380eac76fe6180ca98b20aa03c08097bda1fbbce895"],
            header: "020000002cfe7e6d3752df408c52a8c09203aa3852a3e1a6d42dfdec233d1e5b0000000095e8bcfba1bd9780c003aa208ba90c18e66fc7ea8073beb086070e861decb9a0f2cdd450ffff001d05872d97",
            hex: "020000002cfe7e6d3752df408c52a8c09203aa3852a3e1a6d42dfdec233d1e5b0000000095e8bcfba1bd9780c003aa208ba90c18e66fc7ea8073beb086070e861decb9a0f2cdd450ffff001d05872d970102000000010000000000000000000000000000000000000000000000000000000000000000ffffffff0a51010f062f503253482fffffffff020bb326ed050000002321036c370a402c3e50063d3abd46d49ad174aae679bdc25f2a4ce5a3a4ca15b8b86eaccc60948c0b0000001976a91485e54144c4020a65fa0a8fdbac8bba75dbc2fd0088ac0000000001000000",
        },
        BlockFixture {
            hash: "a22f538ea6b27bdb17f776769a0be6c8069e61c9e843b163f275d616039b48bc",
            merkle_root: "d044101d844bc9c60547cf71e8161e8796efc4bf8fd0764ad11659ecda53ce8d",
            txids: &["04ccbd5286b0d0aa26d4e6ce0a3cbb015072c70f297fb6a81bf91e292f044b2b", "af5e56389d6b375f230936f0194193b76624ae0f3cde90d87857b0e5118b5b3c"],
            header: "00000020abeb1b9483c60b0db5c7637f7a0d38f7d267d323361363e0c11bba439fbc6a608dce53daec5916d14a76d08fbfc4ef96871e16e871cf4705c6c94b841d1044d0b6d8ed6affff801d00000000ff010000002051adb4a131a743e0edc8e42059adec8a7dbc564cd54ea69668a406fa95bcd21003bc0100edb20019007b3f47d7a4d65ed03285ae26d3bb4818f8d28900e0a273d037bba230925b4af1e81dbb7fb74fb044fb6cc085202c5fd589ed818c188dbe09c2db1645214487dc79965f0000000000000000252458000000000635e5a4718203829f975e9c45eeb012e08bab80974a7fa26df356d351475b06d45223ff44faa605752a43ca0fd82ad19e9eff0b395f6c378ead154d151add8790a1cfc3b89de1f01284b64c0bcd1518d4192875d64931dfa8112782e1e864e1dba71a633025f2cf09fe07081a55967bff44e3db19f0b9d67ed5cbf87a48d707456f97f74f371763ba47709a4bcc624200ba8ead941a14d61fc511acaca0cf8fbcbf487547fdd9c08341cc64137ecd784c5c75c7a3d7422d401652610d445b3bce9b6100a03320976e1e6792b25030d5a27d71a58f7fb9a07a4044811701000000000000000000ace50117cc41d674",
            hex: "00000020abeb1b9483c60b0db5c7637f7a0d38f7d267d323361363e0c11bba439fbc6a608dce53daec5916d14a76d08fbfc4ef96871e16e871cf4705c6c94b841d1044d0b6d8ed6affff801d00000000ff010000002051adb4a131a743e0edc8e42059adec8a7dbc564cd54ea69668a406fa95bcd21003bc0100edb20019007b3f47d7a4d65ed03285ae26d3bb4818f8d28900e0a273d037bba230925b4af1e81dbb7fb74fb044fb6cc085202c5fd589ed818c188dbe09c2db1645214487dc79965f0000000000000000252458000000000635e5a4718203829f975e9c45eeb012e08bab80974a7fa26df356d351475b06d45223ff44faa605752a43ca0fd82ad19e9eff0b395f6c378ead154d151add8790a1cfc3b89de1f01284b64c0bcd1518d4192875d64931dfa8112782e1e864e1dba71a633025f2cf09fe07081a55967bff44e3db19f0b9d67ed5cbf87a48d707456f97f74f371763ba47709a4bcc624200ba8ead941a14d61fc511acaca0cf8fbcbf487547fdd9c08341cc64137ecd784c5c75c7a3d7422d401652610d445b3bce9b6100a03320976e1e6792b25030d5a27d71a58f7fb9a07a4044811701000000000000000000ace50117cc41d6740202000000ff01010000000000000000000000000000000000000000000000000000000000000000ffffffff51038b96071f56696e74616765486173682e636f6d202f2f207374696c6c20747279696e670cb85abea03f7a08b3000000007974d4264fa9a3ddddb3de4672c3c253436189f7a3222ce792a2de71f5aaec6cffffffff03a4525d51010000001600145f5c7aef50efba4452d90ffe9b462daaf0be273558d411e70000000016001469bda0706e71caa40f9b35e2cc1a6013f261ae920000000000000000266a242f89c7e5ead1b8c78a0d051161cdc8dc75f09c899a051f47868869e2218590f94b4a49480100edc7ed6a8b96070002000000017962f88430dde5366e0eafac0437461df02a0560e0e1a688c6a29d97486c97b10000000000ffffffff010000000000000000262501196561dc39b420d58be52329bce006308f1461d3975973c35ff2f87e5d73fedd4b4a4948edc7ed6a8b960700",
        },
    ];

    // Mainnet transactions, with getblock's prevout values and fees.
    const TXS: &[TxFixture] = &[
        TxFixture {
            txid: "bc0f8cd20977877c22f0245e2f73374f5f250ec21cb3ff4240815f52e9770b05",
            wtxid: "bc0f8cd20977877c22f0245e2f73374f5f250ec21cb3ff4240815f52e9770b05",
            size: 930,
            weight: 3720,
            lock_height: 497485,
            fee: 10000000,
            prevouts: &[(6303690000000, 85306, 4254811510631), (7200100000000, 92428, 4892983660970), (3800000000000, 86551, 2567939119849), (8181810000000, 85303, 5522473308140), (100000000, 92375, 67953727), (1141000000000, 92751, 775630110668)],
            hex: "020000000600dcda4077d6b09a4e7b32532abe9e7037c37992e30f2da9e4f239c341037bd5000000006a473044022058c47d1b7e169b0ffb5ec258b1f9b6ca31d4555ecd8187d05483683766e92d0d022002540dfa18f57223da2b90ce9b359164dc7f76c786cc126cb30b2c4d6e50287901210365fba47c8738ec03e8251a9ce5c8a3bfb73657e5c5da0aa5b8ad342997565dc2fdffffff149cd1537c78e9253825f1b645b9857eeaaa3426b8ccdb1787a6b75e3f1e7f1c000000006a47304402204f10771207cb9ea45c3aa55136698e208a790c75cc0c30c30cd9d35e840f728a0220263a54a8f3d87f48542acb866fd16684bd22805c2dbf6e0513e0c9a3a64b2f7a012102c5c195f3d2f7a34e6c6a98c44f348513ee25fec1a91d596add49074edb83fcecfdffffff94294edf10fa398b13374084d7939dcd3d4fe38d6040573e40af6b0e884a8348000000006a473044022036a5f53df52e8b303e36085be0eede6681a0b2ff44e109bac4a28e2c13fe117a02202209c21b38817208caae8eae28feeac8fe72a32b4e7733e890e8649482ec982a0121037aa589cd08ef156683bc3c6fa9ea838907e2957385e89d0111f034af0c28e935fdffffffacbd32c554d8a0c1f3da7dd7725a9aa8cf478dedd433bfa62f33f30151580d25000000006a47304402203ba218a7a1ab84d1d6b58432258065533a258d5912da2d0683c4085549df90a7022035efbf2a413f79085405260b2c99abcbf4ffb156513d2326f96270935382749b0121025e257e07073c831a340fb34e50144a25bbc62a9b6b930acfa26a3332dfbb97ebfdffffffcc2514c751435d95e20c5566389550466f6245f6e3b0d7be9c3ae6776cd7d6b6010000006a4730440220799d1bca379181dd4bca0a586e8e7d0238161466859e3f196e7648592e87f95402204784260a46228e96e87915952745a628c49b74468fbee0d356c0e2884a7f352c012102c5c195f3d2f7a34e6c6a98c44f348513ee25fec1a91d596add49074edb83fcecfdffffffde2ab214c84d9391ee03ead98f574e82a8c2727b03f9f5dfc38f64d16f6dd511000000006a4730440220415fda7dcd5e30ec5271c03f31ece0f524ec4090fe1720469ca87a58caabd27802205cad3802fad02560937532d110fa4c962d00f6362dd46f68e3402dd152febc5c012102c5c195f3d2f7a34e6c6a98c44f348513ee25fec1a91d596add49074edb83fcecfdffffff017121da2f621000001976a914b23f5099255a0f6d6a78963be97dc5e69e5db5f588ac000000004d970700",
        },
        TxFixture {
            txid: "6c8786a368e7607776e7d0cdc36ec3e554c914ff6f99204c178de553d8dfd267",
            wtxid: "6c8786a368e7607776e7d0cdc36ec3e554c914ff6f99204c178de553d8dfd267",
            size: 229,
            weight: 916,
            lock_height: 497480,
            fee: 100000,
            prevouts: &[(301094886, 93787, 204881880)],
            hex: "0200000001af6b7ff025f0077ca90a0d43b6cea08bc04eaa9889f90c2d07e7d4d311b27f90000000006a47304402201f2253c2f0fcd28fd3fbbd1b268e053104f44d360390ea63ba8f7b3c6e68f71002201462527beafc29d57df2d1418010e8834c38419443c6eaadbba1cf8afb2f4c61012102be3cefd4088c2e7fea14826e98df199844f6197b3673e33c8d9cff1c9b706174fdffffff0200e1f505000000001976a914a1030add7d227a0a43d10696305bb5574e5ef59288ac38d83e06000000001976a91422794f569b6b6ec2ad115fc14b380aa0461d55c188ac0000000048970700",
        },
        TxFixture {
            txid: "61c765dd5c862af8cd3e726ebdb2b42a4d81e8d25c373dce02769968e974dbe5",
            wtxid: "7376f26386bd505ba81cc5e54229780684c12bc94da491ac92bcb00c8e4ca064",
            size: 351,
            weight: 732,
            lock_height: 497434,
            fee: 3660,
            prevouts: &[(5660037796, 497291, 5659265958), (2897009011, 497332, 2896727218)],
            hex: "02000000ff01022b4b042f291ef91ba8b67f290fc7725001bb3c0acee6d426aad0b08652bdcc040000000000fdffffff212b09eb3cc363dc06f611b7463f77cfa8e76424c58d85228a6e9b9e770b55bc0000000000fdffffff010c0afafd01000000160014b8dd2e198717803f93555f8d46fc48e78fcb2dca0347304402202d154df5d2f4477b21735a72a93f0211861645069c5c1a199bd9c9b5bf9d4c7a02207c7332e64d6cee78b4217ddd5aae44d8192715a566368dd1e4124989725615c70124002102f51d9b4fbad9ac386d9dfdd2b67f49a966ee67309cc0a90e91b47fb8361da069ac000347304402206a992269ebdb97afc92fc3bfd080dbea5d3fc687d78d5f0a287602ec12d4483502205bcb3cbc74eabee956684df653827af246e8d3b779930083305fd3c3d67f30530124002102f51d9b4fbad9ac386d9dfdd2b67f49a966ee67309cc0a90e91b47fb8361da069ac00000000001a970700",
        },
        TxFixture {
            txid: "7e99d7a082fdf78825d8aca00a0fe54d0ce854e5379b363acf4f154d2991b432",
            wtxid: "7e99d7a082fdf78825d8aca00a0fe54d0ce854e5379b363acf4f154d2991b432",
            size: 192,
            weight: 768,
            lock_height: 497356,
            fee: 1920,
            prevouts: &[(99997830, 497354, 99997639)],
            hex: "0200000001cfc8e62dd2a46ed34e53bbe08fdbf428abe7318a427617b8196972c04875c2ea000000006a473044022037e6e52a828f126b6ed4943b1bc371b858b2fec4b5ca69e13e1bdfffaa702fb002202a4d193189d5c58cde894782ae00d7edcf9c489cc6b92440d5281f0bdb73caeb012102c3d6049ac67d9b946b5f1bbb8c89fc7ecc8e1a49c4e1bfe05d57305cb71e31fefdffffff0147d0f5050000000016001469bda0706e71caa40f9b35e2cc1a6013f261ae929a960700cc960700",
        },
        TxFixture {
            txid: "eac27548c0726919b81776428a31e7ab28f4db8fe0bb534ed36ea4d22de6c8cf",
            wtxid: "5d1a195b91ca1137bcbd1f8ed63bad75ffe1d59a7713851fad80258185a9d361",
            size: 385,
            weight: 868,
            lock_height: 497354,
            fee: 2170,
            prevouts: &[(149945410, 497011, 149896369), (49980542, 497011, 49964195)],
            hex: "02000000ff0102351d88393d38bb2af0a54b31dc075fff90549191509ffbc51cb029e6a83840c20000000000fdffffff5dcc59d96a32c23602eeb408fdfbef3d93aa97809b7006aa4b4b0116f49e70350000000000fdffffff0286d8f505000000001976a914056e884f7fa82b31a433ea1bfeb1aca7f24b46f788ac54c0f30500000000160014427b99e0534b44088486529fb55be296cd5fce9a034730440220168871c1e4c3e19b3a2fc973181491f48648646b8c605d0df30c3ae3cc3a45e10220264a656cc5cd7c4cb33611bbb4adb18a60e2961c1a50a1171d0886fb88da6f4701240021022b1dab0ef835a73045c427f28b596960dddbf72b5357f1f7d715590396a1edc4ac000347304402205eb6929bfe1e61358794004e8e177dc83e79fb1f858ad79464dd5cef5c9ced4f02201ae91bfca5eab447a159eaef0fd3d28193069925090fe383955d29faa1f23cf901240021022b1dab0ef835a73045c427f28b596960dddbf72b5357f1f7d715590396a1edc4ac00c9960700ca960700",
        },
        TxFixture {
            txid: "dd43e129aed34372ef70129b796bdaea88e4f3d23ec49453c37b50013be53967",
            wtxid: "7eaaa213fbb9953bdfa45902811fcfbf45239bd0d134a98117ec36916d925914",
            size: 230,
            weight: 581,
            lock_height: 497294,
            fee: 1460000,
            prevouts: &[(10545852744, 483769, 10410701073)],
            hex: "02000000ff01012d9d5c91e74c027fe1118827e2957174d687ba2525b5fbb38296bb698f7258e10100000000fdffffff0200222048020000001600146d666ac2aaccecb7729ea5e69557802da3b273bbf147502400000000160014a1939240f3bd18f5415189a1631a1b6101f553c60347304402202dd1644ee7fb0b625c177aef8c65a5de5a7a6061b7767dba03491cdf235a97ec022017bb802d3a128f7dc6d9a5d53d285cc31bb9224fef311c2a85226b37059fcb4701240021025f777b3e5b68cd9187270cf02e2094264e0d8f3db50b873d8748beeba6bd9975ac008d9607008e960700",
        },
        TxFixture {
            txid: "4ae080d89abb7a3ab1ca5bf1e9e09fb135cf6053002d157da42b270151d7e533",
            wtxid: "99cea7843e5680cdf9fa219db4621ace98791537496e5bd5599846e14b5432ca",
            size: 230,
            weight: 581,
            lock_height: 497151,
            fee: 2920,
            prevouts: &[(7327367672, 496661, 7323944388)],
            hex: "02000000ff01017f04caacc6a092daf990ac476c5a275e20d64701c4c7004c35ff099be4dbe9e80100000000fdffffff0200e1f50500000000160014b8dd2e198717803f93555f8d46fc48e78fcb2dca5c9994ae01000000160014260c6537d51384de29e15e4fe9567c6f4c239fa1034730440220367469e11de5d921b089ee91970ff049be9a0eceb743e07f25b3937d69a4612b022004255b472f6febd92c4ba7d56e9a7aa5d8649345c3a04547e1b97dffc91bde350124002102f4ea9cbfb40eddd8f1cb724f6cfb4993804d5760003afccaee0baa5a67b71b9dac00fe950700ff950700",
        },
        TxFixture {
            txid: "7a57cbc29852a39f3fca7c75d3a8c0a6a09a2717e08b2f49d5360fbc0ce647da",
            wtxid: "4009c8dde93a7ed9afaeeba58312868ca169531b693f3b75f70bf80c94674716",
            size: 1871,
            weight: 3482,
            lock_height: 497151,
            fee: 17420,
            prevouts: &[(7223941468, 497151, 7223941468), (699887741, 495734, 698942581), (9536743164, 496062, 9526843903), (1299939689, 495884, 1298369912), (300000000, 495734, 299594866), (9035697155, 495722, 9023391682), (607469333333, 495343, 606422810477), (100000000, 495938, 99884386), (972625500000, 495698, 971278676468), (9536743164, 496287, 9528888362), (9536743164, 495998, 9526262448), (1333649333333, 496020, 1332211626437)],
            hex: "02000000ff010c33e5d75101272ba47d152d005360cf35b19fe0e9f15bcab13a7abb9ad880e04a0100000000fdffffffcf9830713b26964592e7f72fb25ea233afe9f8aa3f0df38998d186e998b0509f0000000000fdffffffb55fdacf06a1095424ed8bf260fb4b6d4fafeb74752320e5abba70ad8d41821f0000000000fdffffff27a5ced3d65c280dd53322221d82ef61ba46db9154482a73bc455a2d2a16cc2d0000000000fdffffff47e43fc65c5f5fdad3ab328eb6d198e8eaf291f05e11300b6a6b39894b66a0090000000000fdffffff582d42a679fbed5df965c8a681f390268861bdc8a3ef2a7339b1142afe2b4c3c0100000000fdffffff704cdadf06db162c637eda57b4c493d021d820751331ade564c766aae0978c620100000000fdffffff81159048b07c9505ea8835511d6e4902086146ec75b7e459ef83040349a3c2b40000000000fdffffff19bcb64a476aeb574bb62ed705c65ec1e3d3af6ec1e1b9ce707d9a827f602a6a0100000000fdffffff24bd8aa0d8e2bf3061a9ae5b014bd1bc29950b9bcb958b1b5724a6e47e0f70790000000000fdffffff5da0fad14f80ee9bc89a31762ebff87c44d7df6f067a155bc5340d3b634aacf60000000000fdffffff94829074b4c6cb98c2eb9afefc83a0fc9cb18123fa99a310881cccdcc4b8ab450100000000fdffffff01d2843b83b0020000160014b8dd2e198717803f93555f8d46fc48e78fcb2dca0347304402206b83b0b06ca301e03e06430d01db912b63c136b11c5780a31e5709444a70fd21022072098275ec41b0694f8c9ff27242cd551a8dc37f7cb51b55456edc98976c71960124002102216d412fa54dbc6d6b6ac89a83daf92986c3a7b90912bfc86c2638538e025898ac000347304402207e1c837748bf49cfd7f67e09d12be6079a0730ee03fa25fe3ced9184d14e4d870220070ac5248a3ffe4a488ae9ea7acc57087faa4c59ceda91c8eb8e6393a1648b3a0124002102f51d9b4fbad9ac386d9dfdd2b67f49a966ee67309cc0a90e91b47fb8361da069ac000347304402202caf70f75bd0a740b2e3b89de24a6b37b5eac3c64fb80f4e289d35f19e58343c02205b3c03170b05df866f7cafe5d181a4ea0414b720478666306ae674fe079a65520124002102f51d9b4fbad9ac386d9dfdd2b67f49a966ee67309cc0a90e91b47fb8361da069ac0003473044022067a3acf9faff609f167b863f169eaa33942b1612d1c69ce1e0d66861d1fa2c5702200ec630350c03c9fb2004a2623cda23c9376990a734845530ce6fc5657f56651a0124002102f51d9b4fbad9ac386d9dfdd2b67f49a966ee67309cc0a90e91b47fb8361da069ac000347304402204cf32df11bc53720ee1cf1551bf5368a9f4fa63dcc585d18bd236633cf494432022018fcf1a284dafb5b48047ba08e447b5f9a83275fe225278cc97d674ffcec06f10124002102f51d9b4fbad9ac386d9dfdd2b67f49a966ee67309cc0a90e91b47fb8361da069ac000347304402204f4d61eb611aac2f82540181bf45898f1866a430f6d1e5ef529ef8e56263179a02202d847a25310e367e619f3a392325fb910d1359e640cde3d4dbbf853a502c0a6801240021031ae3f424c1afc4c7e9456f7af04b9bd67130abd78d7c5d978380e77d4a8c33f0ac000347304402206b7c6231e4b6b1301f522312fa5c527e5978e6642ed2b6acbf899da00ce87ed502203aede44c70d899e9986f9238dc9918df56a17eb9dc12ce3b8eccaceff1b73fa50124002102f51d9b4fbad9ac386d9dfdd2b67f49a966ee67309cc0a90e91b47fb8361da069ac0003473044022054eb16bf8c87ebc8c4c4afb3910fd4b12c999052f4c4eac382b3f07d3029704802205abdf393298458e6a1d46007497222c05194fccb1111a526ee4f37aea640b3db0124002102f51d9b4fbad9ac386d9dfdd2b67f49a966ee67309cc0a90e91b47fb8361da069ac000347304402206c39ef7b38806d529d4aaea63ee2e99a186a8ef4b9539a1b78701e1e0152361f022069cd611fcab8aecc61ca12394292bfaa96616531722ccfcac7e71c8941f8afaf0124002102f51d9b4fbad9ac386d9dfdd2b67f49a966ee67309cc0a90e91b47fb8361da069ac00034730440220219206aec6360501e78fb403344e830110b8dc8b39cc9b00c883e351a9a9e9bd02201a75e0db4d0f68184977af8b8a83246c1e27b54d43f1ffe0efa34a04ce7345a80124002102f51d9b4fbad9ac386d9dfdd2b67f49a966ee67309cc0a90e91b47fb8361da069ac00034730440220667d845e7517f5b37f1cdc4bdb26ffbaf41943b0a6bb982e81c53b80aa417381022021919a7e204c03f8337c241961c3ae3c9dd9661e8fd76c302c40c0221d23c91a0124002102f51d9b4fbad9ac386d9dfdd2b67f49a966ee67309cc0a90e91b47fb8361da069ac00034730440220341af1bf67d0791ecc1924ce55e2ec31b15410d8e1cb8530833275dbce188b2502207390ab4f1b587cdce2fc474c0d01fa67bae8e2d0acb8f39824c5cc96abba55310124002102f51d9b4fbad9ac386d9dfdd2b67f49a966ee67309cc0a90e91b47fb8361da069ac0000000000ff950700",
        },
    ];

    #[test]
    fn mainnet_blocks() {
        for f in BLOCKS {
            let raw = Vec::<u8>::from_hex(f.hex).unwrap();
            let block: Block = deserialize(&raw).unwrap();
            assert_eq!(block.block_hash().to_string(), f.hash);
            assert_eq!(block.header.merkle_root.to_string(), f.merkle_root);
            assert!(block.check_merkle_root());
            let txids: Vec<String> =
                block.txdata.iter().map(|tx| tx.compute_txid().to_string()).collect();
            assert_eq!(txids, f.txids);
            // getblockheader's serialization, the extended one for a merge-mined block
            let raw = Vec::<u8>::from_hex(f.header).unwrap();
            let header: BlockHeader = deserialize(&raw).unwrap();
            assert_eq!(header.block_hash().to_string(), f.hash);
            let (_, aux_pow) = BlockHeader::decode_with_aux_pow(&mut raw.as_slice()).unwrap();
            assert_eq!(aux_pow.is_some(), raw.len() > 80);
        }
    }

    #[test]
    fn mainnet_transactions() {
        for f in TXS {
            let raw = Vec::<u8>::from_hex(f.hex).unwrap();
            let tx: Transaction = deserialize(&raw).unwrap();
            assert_eq!(tx.compute_txid().to_string(), f.txid);
            assert_eq!(tx.compute_wtxid().to_string(), f.wtxid);
            assert_eq!(tx.total_size(), f.size);
            assert_eq!(tx.weight().to_wu(), f.weight);
            assert_eq!(tx.lock_height, f.lock_height);
            assert_eq!(bitcoin::consensus::serialize(&tx), raw);
            for &(value, refheight, adjusted) in f.prevouts {
                assert_eq!(value_at(value, refheight, tx.lock_height), adjusted, "{}", f.txid);
            }
            let txos: Vec<TxOut> = f
                .prevouts
                .iter()
                .map(|&(value, _, _)| TxOut {
                    value: bitcoin::Amount::from_sat(value),
                    script_pubkey: Default::default(),
                })
                .collect();
            let prevouts = txos.iter().zip(f.prevouts.iter().map(|p| p.1));
            // getblock's fee, which leaves out the per-input adjustment of the early chain
            assert_eq!(tx_fee(&tx, prevouts, 0), f.fee, "{}", f.txid);
        }
    }

    #[test]
    fn witness_commitments() {
        // the block-final output of block 497521
        let script = Vec::<u8>::from_hex(
            "250196819b2df5f13c7a3522d44d9360472b0e168dbeba6c1929549e66b08eebcf834b4a4948",
        )
        .unwrap();
        assert!(is_witness_commitment(&script));
        assert!(!is_witness_commitment(&script[..script.len() - 1]));
        let mut other = script.clone();
        other[34] ^= 1;
        assert!(!is_witness_commitment(&other));
        let p2pkh = Vec::<u8>::from_hex("76a914a1030add7d227a0a43d10696305bb5574e5ef59288ac").unwrap();
        assert!(!is_witness_commitment(&p2pkh));
        assert!(!is_witness_commitment(&[]));
    }

    #[test]
    fn early_mainnet_input_adjustment() {
        assert_eq!(per_input_adjustment(Network::Bitcoin, Some(61_823)), 2);
        assert_eq!(per_input_adjustment(Network::Bitcoin, Some(158_424)), 2);
        assert_eq!(per_input_adjustment(Network::Bitcoin, Some(158_425)), 1);
        assert_eq!(per_input_adjustment(Network::Bitcoin, Some(229_375)), 1);
        assert_eq!(per_input_adjustment(Network::Bitcoin, Some(229_376)), 0);
        assert_eq!(per_input_adjustment(Network::Bitcoin, None), 0);
        assert_eq!(per_input_adjustment(Network::Regtest, Some(0)), 0);

        // Block 61823 confirms two transactions whose outputs exceed their inputs' adjusted values,
        // by 4 and 1 kria, valid only by the 2 kria then credited to each input.  The (value,
        // refheight) of their spent outputs, and their fees.
        let cases: [(&[(u64, u32)], u64); 2] = [
            (
                &[
                    (19418710263, 61150),
                    (19419095028, 61146),
                    (19380876956, 61583),
                    (19418907649, 61148),
                    (19419391102, 61143),
                    (19418414191, 61153),
                ],
                8,
            ),
            (&[(19383132715, 61511), (19419588484, 61141)], 3),
        ];
        let hexes = include_str!("../tests/freicoin/mainnet-61823.txt");
        for (hex, (inputs, fee)) in hexes.lines().zip(cases) {
            let tx: Transaction = deserialize(&Vec::<u8>::from_hex(hex.trim()).unwrap()).unwrap();
            let txos: Vec<TxOut> = inputs
                .iter()
                .map(|&(value, _)| TxOut {
                    value: bitcoin::Amount::from_sat(value),
                    script_pubkey: Default::default(),
                })
                .collect();
            let prevouts = || txos.iter().zip(inputs.iter().map(|p| p.1));
            assert_eq!(tx_fee(&tx, prevouts(), 0), 0, "outputs above the adjusted inputs");
            let adjustment = per_input_adjustment(Network::Bitcoin, Some(61_823));
            assert_eq!(tx_fee(&tx, prevouts(), adjustment), fee);
        }
    }

    /// Mainnet addresses and their scripts, from freicoind's validateaddress and decodescript:
    /// every witness version takes the Bech32m checksum, and OP_1 is witness version 2.
    #[test]
    fn mainnet_addresses() {
        use bitcoin::{Address, Network as BNetwork, ScriptBuf};
        use std::str::FromStr;
        let vectors = [
            ("1FgMSH5nUEieiWyX9YZuFvvUihWtJ2bRrZ", "76a914a1030add7d227a0a43d10696305bb5574e5ef59288ac"),
            ("fc1qq9kpkhf43g7x8wrytj4up0s47uvkzknvxa3lkn", "0014016c1b5d358a3c63b8645cabc0be15f719615a6c"),
            (
                "fc1q424242424242424242424242424242424242424242424242424q9ejjz5",
                "0020aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ),
            (
                "fc1zhwamhwamhwamhwamhwamhwamhwamhwamhwamhwamhwamhwamhwasf6h4m0",
                "5120bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            ),
        ];
        for (address, script) in vectors {
            let parsed = Address::from_str(address)
                .unwrap()
                .require_network(BNetwork::Bitcoin)
                .unwrap();
            assert_eq!(parsed.script_pubkey().to_hex_string(), script, "{}", address);
            let script = ScriptBuf::from_hex(script).unwrap();
            let shown = Address::from_script(&script, BNetwork::Bitcoin).unwrap();
            assert_eq!(shown.to_string(), address);
        }
        // the same version 0 program under Bitcoin's Bech32 checksum is no Freicoin address
        let hrp = bitcoin::bech32::Hrp::parse("fc").unwrap();
        let program = Vec::<u8>::from_hex("016c1b5d358a3c63b8645cabc0be15f719615a6c").unwrap();
        let bech32 = bitcoin::bech32::segwit::encode_v0(hrp, &program).unwrap();
        assert!(bech32.starts_with("fc1q") && bech32 != vectors[1].0);
        assert!(Address::from_str(&bech32).is_err());
    }
}
