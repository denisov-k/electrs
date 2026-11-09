# electrs for Freicoin

This branch adapts [Blockstream's electrs](https://github.com/Blockstream/electrs) (the
`new-index` branch, the backend of the [Esplora](https://github.com/Blockstream/esplora) block
explorer) to [Freicoin](https://github.com/tradecraftio/tradecraft), as run behind
https://freicoin.ru/explorer/. The Esplora frontend's Freicoin flavor is in a branch of its own of
the esplora repository.

It indexes Freicoin's mainnet, testnet, signet and regtest from freicoind 28.1, and serves
Esplora's HTTP REST API and the Electrum protocol, with Freicoin's demurrage taken into account.

## What is different from upstream

### Formats

The `bitcoin` crate (rust-bitcoin 0.32.8) is vendored under `freicoin/bitcoin` and patched in
through `[patch.crates-io]`, as is `corepc-types` under `freicoin/corepc-types`:

- **Transactions.** The extended serialization's marker is `0xff`, not `0x00`, followed by the flags
  byte. Transactions carry `lock_height` (the reference height of their output values) after
  `nLockTime`, except a version 1 transaction with a single input that has a null prevout. The txid
  covers `lock_height`.
- **Block headers.** Bit 23 of `nBits` marks an extended header, followed by `0xff`, a flags byte
  and the auxiliary proof-of-work of a merge-mined block. `Header::decode_with_aux_pow` reads the
  header together with its `AuxPow`. The block hash is that of the 80-byte header.
- **Network parameters.** The magic bytes and the address prefixes (`fc`, `tf`, `fcrt`) are
  Freicoin's, as are the genesis blocks, ports and data directories in `src/chain.rs` and
  `src/config.rs`. electrs keeps rust-bitcoin's network names: `--network mainnet` is
  `Network::Bitcoin`.
- **Addresses.** Every witness version, 0 included, takes the Bech32m checksum. Versions run from 0
  to 30 on the 31 single-byte opcodes that could start a script before the protocol cleanup:
  `OP_0`, `OP_1NEGATE`, `OP_1` to `OP_16`, `OP_NOP`, `OP_DEPTH`, `OP_CODESEPARATOR` and `OP_NOP1`
  to `OP_NOP10`. A witness program may be followed by a shard prefix and an extension output.
  `src/freicoin.rs` (`witness`) gives these outputs their addresses and types as freicoind does:
  `witness_v0_shorthash`, `witness_v0_longhash`, `anchor` and `witness_unknown`.

### Demurrage

A Freicoin output's value is denominated at its reference height, the `lock_height` of the
transaction that created it, and decays by a factor of (1 - 2^-20) per block after it.
`src/freicoin.rs` ports `TimeAdjustValueForward` (consensus/amount.cpp) exactly. Its test vectors
come from the C++.

- **Fees.** A transaction's fee is its inputs' values adjusted to its `lock_height`, less its
  outputs. The txo rows of the index store each output's reference height after the serialized
  output, which makes the database incompatible with upstream's.
- **Early mainnet blocks.** Until the truncation (height 158425) and ALU (229376) soft forks,
  consensus credited each input one kria per fork on top of its adjusted value
  (`per_input_adjustment` of `CheckTxInputs`). Some early transactions rely on it. Two in block
  61823 have outputs 4 and 1 kria above their adjusted inputs, so their fees are 8 and 3 kria.
- **Present values.** The balances and utxos below are valued at the next block's height.

### API additions

REST:

- `GET /tx/:txid` adds `lock_height`. Its `fee` takes demurrage into account.
- `GET /address/:address` (and `/scripthash/:hash`) adds `present_value`: `{height, confirmed,
  spendable}`. These are the unspent outputs' value after demurrage at `height`, the confirmed ones'
  and those spendable counting the mempool. It is null past `--utxos-limit`. The `chain_stats` and
  `mempool_stats` sums stay face values.
- `GET /address/:address/utxo` adds each output's `refheight` and `present_value`.
- `GET /block/:hash` of a merge-mined block adds `parent_block`. This is the parent chain (bitcoin)
  block its auxiliary proof-of-work commits to, reconstructed as consensus does it (`aux_pow` in
  `src/freicoin.rs`). It gives the parent's hash, previous block, version, Merkle root, time, bits,
  nonce and transaction count. It also gives `meets_target`, whether the parent's hash meets the
  block's auxiliary target, which a correct reconstruction does, and `is_parent_chain_block`,
  whether it also meets the parent chain's own target (a bitcoin block rather than a pool share).
  electrs reads the extended header from freicoind's `getblockheader` and caches it.
- Block sizes count a merge-mined header's auxiliary proof-of-work, as freicoind's do.
- The single output of each block's block-final transaction, which carries the block's witness
  commitment, has the type `witness_commitment`.
- `GET /block-template` (`--enable-mining-rest`) asks freicoind for the `finaltx` and `auxpow`
  rules, which it requires clients to declare.

Electrum:

- `blockchain.scripthash.get_balance` returns present values (face values past
  `--utxos-limit`).
- `blockchain.scripthash.listunspent` adds each utxo's `refheight`.

## Building and testing

As upstream:

    cargo build --release --bin electrs

The unit tests run their node tests against freicoind, which stands in for bitcoind:

    BITCOIND_EXE=/path/to/freicoind BITCOIND_SKIP_DOWNLOAD=1 ELEMENTSD_SKIP_DOWNLOAD=1 \
        ELECTRUMD_SKIP_DOWNLOAD=1 cargo test --release --lib

The tests in `src/freicoin.rs` use mainnet data read from freicoind: blocks, a merge-mined one
among them, transactions with their fees and prevouts, addresses, and the decodescript of witness
programs.

## Running

Against a mainnet freicoind with `server=1` and the RPC reachable:

    electrs --network mainnet --daemon-dir ~/.freicoin --daemon-rpc-addr 127.0.0.1:8638 \
        --db-dir ./db --http-addr 127.0.0.1:3000 --electrum-rpc-addr 127.0.0.1:50001 \
        --jsonrpc-import --utxos-limit 20000 --history-scan-limit 3000000 --enable-mining-rest

- **`--jsonrpc-import`** fetches blocks through RPC. Mainnet, about 500,000 blocks and 1.4 million
  transactions, indexes in some ten minutes into about 1.2 GB.
- **Importing from blk files** (without `--jsonrpc-import`) needs much more memory than for
  bitcoin. electrs reads and parses a whole blk file at a time, and freicoind keeps the whole of
  mainnet in a single 512 MB file. Limited to 1.5 GB and to 2.5 GB, the import used all of it and
  wrote nothing.
- **`--utxos-limit` and `--history-scan-limit`** are raised for the pool and exchange addresses,
  which hold over 10,000 utxos and 100,000 history rows.

## Verifying against freicoind

`contrib/freicoin/` compares a running electrs with freicoind:

- `verify.py` checks blocks and transactions: hashes, sizes, transaction counts, Merkle roots,
  txids, merge-mined parents, transaction sizes, weights, lock_heights and fees.
- `verify_balances.py` checks addresses' utxos, reference heights and balances after demurrage
  against `scantxoutset`.
- `smoke_networks.sh` checks the genesis blocks and addresses of testnet, signet and regtest.

Both Python scripts take `FREICOIND_RPC`, `FREICOIND_RPC_AUTH` (or `FREICOIND_COOKIE`) and
`ESPLORA_URL`.

## Known limitations

- **Shard prefixes and extension outputs.** An address carries only a witness program's version
  and program, as freicoind's does, so the address of a script with a shard prefix or an extension
  output finds the plain program's script, not that one.
- **freicoind's `getblock`.** With verbosity 2 or 3, `getblock` fails with "Internal bug detected:
  MoneyRange(fee)" on some early mainnet blocks (61823 is one). `TxToUniv` leaves out the per-input
  adjustment above, so it computes their fees below zero. electrs is unaffected, and `verify.py`
  works around it.
- **The `liquid` feature.** It is untouched but not built or tested in this branch.
