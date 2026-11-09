#!/bin/bash
# Smoke test of electrs on Freicoin's testnet, signet and regtest: for each, a throwaway freicoind
# with no peers (only the genesis block) and electrs against it, comparing electrs' genesis block
# hash and an address of a version 0 witness program with the node's.
#
#   FREICOIND=/path/to/freicoind ELECTRS=target/release/electrs ./smoke_networks.sh
set -u
FREICOIND=${FREICOIND:-freicoind}
ELECTRS=${ELECTRS:-target/release/electrs}
SCRIPT=0014cccccccccccccccccccccccccccccccccccccccc
status=0

for net in testnet signet regtest; do
  case $net in
    testnet) port=18638;;
    signet) port=38638;;
    regtest) port=28638;;
  esac
  dir=$(mktemp -d)
  "$FREICOIND" -$net -datadir="$dir" -server -rpcuser=u -rpcpassword=p -rpcport=$port \
    -connect=0 -listen=0 -maxtipage=2000000000 -minimumchainwork=0 -printtoconsole=0 &
  node=$!
  rpc() { curl -s --user u:p -d "{\"method\":\"$1\",\"params\":$2}" http://127.0.0.1:$port/; }
  for _ in $(seq 1 30); do
    genesis=$(rpc getblockhash '[0]' | sed -n 's/.*"result":"\([0-9a-f]*\)".*/\1/p')
    [ -n "$genesis" ] && break; sleep 1
  done
  address=$(rpc decodescript "[\"$SCRIPT\"]" | sed -n 's/.*"address":"\([a-z0-9]*\)".*/\1/p')

  "$ELECTRS" --network $net --jsonrpc-import --daemon-dir "$dir" --daemon-rpc-addr 127.0.0.1:$port \
    --cookie u:p --db-dir "$dir/db" --http-addr 127.0.0.1:13000 --electrum-rpc-addr 127.0.0.1:15000 \
    --monitoring-addr 127.0.0.1:14224 >"$dir/electrs.log" 2>&1 &
  electrs=$!
  for _ in $(seq 1 60); do
    grep -q "REST server running" "$dir/electrs.log" && break; sleep 1
  done
  electrs_genesis=$(curl -s http://127.0.0.1:13000/block-height/0)
  electrs_address=$(curl -s http://127.0.0.1:13000/address/$address | sed -n 's/.*"address":"\([a-z0-9]*\)".*/\1/p')

  if [ -n "$genesis" ] && [ "$genesis" = "$electrs_genesis" ] && [ -n "$address" ] && [ "$address" = "$electrs_address" ]; then
    echo "$net: ok, genesis $genesis, address $address"
  else
    echo "$net: FAIL, node genesis $genesis electrs $electrs_genesis, address $address electrs $electrs_address"
    tail -5 "$dir/electrs.log"
    status=1
  fi
  kill $electrs $node 2>/dev/null; wait $electrs $node 2>/dev/null
  rm -rf "$dir"
done
exit $status
