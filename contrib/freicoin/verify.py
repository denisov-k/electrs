#!/usr/bin/env python3
"""Compares electrs' REST API with freicoind's RPC on mainnet blocks and their transactions:
block hashes, sizes, transaction counts, Merkle roots and txids, merge-mined blocks' parents, and
transaction sizes, weights, lock_heights and fees.

    FREICOIND_RPC=http://127.0.0.1:8638 FREICOIND_RPC_AUTH=user:password \
    ESPLORA_URL=http://127.0.0.1:3000 ./verify.py [random blocks] [seed]

FREICOIND_RPC_AUTH may be left out for FREICOIND_COOKIE, the node's .cookie file.
"""
import base64, json, os, random, sys, urllib.error, urllib.request

NODE = os.environ.get("FREICOIND_RPC", "http://127.0.0.1:8638")
ESPLORA = os.environ.get("ESPLORA_URL", "http://127.0.0.1:3000").rstrip("/")
AUTH = os.environ.get("FREICOIND_RPC_AUTH") or open(os.environ["FREICOIND_COOKIE"]).read().strip()
HEADERS = {"Authorization": "Basic " + base64.b64encode(AUTH.encode()).decode()}


class RpcError(Exception):
    pass


def rpc(method, *params):
    body = json.dumps({"method": method, "params": list(params), "id": 1}).encode()
    try:
        return json.load(urllib.request.urlopen(urllib.request.Request(NODE, body, HEADERS)))["result"]
    except urllib.error.HTTPError as e:
        raise RpcError("%s %s: %s" % (method, params, e.read()[:200]))


def rest(path, raw=False):
    body = urllib.request.urlopen(ESPLORA + path).read()
    return body.decode() if raw else json.loads(body)


def adjustment(height):
    """The kria consensus credits each input on top of its value before the truncation and ALU
    soft forks (mainnet), which getblock's fee leaves out."""
    return (height < 158425) + (height < 229376)


random.seed(int(sys.argv[2]) if len(sys.argv) > 2 else 1)
count = int(sys.argv[1]) if len(sys.argv) > 1 else 50
tip = rpc("getblockcount")
# the genesis and first blocks, an early block under the per-input adjustment, a merge-mined one
heights = [0, 1, 61823, 497291] + random.sample(range(tip), count)
mismatches = 0; txs_checked = 0; fees_checked = 0; merge_mined = 0; node_bug = []


def check(what, electrs, node):
    global mismatches
    if electrs != node:
        mismatches += 1
        print("MISMATCH", what, "electrs", electrs, "node", node)


for height in heights:
    blockhash = rpc("getblockhash", height)
    check(("hash", height), rest("/block-height/%d" % height, raw=True), blockhash)
    try:
        block = rpc("getblock", blockhash, 2)
    except RpcError:
        # freicoind's TxToUniv leaves out the per-input adjustment, so fees below zero trip its
        # MoneyRange check on some early blocks: read those transactions without their fees
        node_bug.append(height)
        block = rpc("getblock", blockhash, 1)
        block["tx"] = [rpc("getrawtransaction", txid, 1, blockhash) for txid in block["tx"]]
    eb = rest("/block/" + blockhash)
    check(("size", height), eb["size"], block["size"])
    check(("tx_count", height), eb["tx_count"], len(block["tx"]))
    check(("merkle_root", height), eb["merkle_root"], block["merkleroot"])
    check(("txids", height), rest("/block/%s/txids" % blockhash), [tx["txid"] for tx in block["tx"]])
    header = rpc("getblockheader", blockhash, False)
    if len(header) > 160:  # a merge-mined block's extended header
        merge_mined += 1
        parent = eb.get("parent_block")
        check(("parent meets the aux target", height), parent and parent["meets_target"], True)
    for tx in block["tx"][:30]:
        et = rest("/tx/" + tx["txid"])
        txs_checked += 1
        check(("size", tx["txid"]), et["size"], tx["size"])
        check(("weight", tx["txid"]), et["weight"], tx["weight"])
        check(("lock_height", tx["txid"]), et["lock_height"], tx["lockheight"])
        if "fee" in tx and height > 0:
            fees_checked += 1
            fee = round(tx["fee"] * 100000000) + adjustment(height) * len(tx["vin"])
            check(("fee", height, tx["txid"]), et["fee"], fee)

print("getblock verbosity 2 failing on the node (MoneyRange(fee)):", node_bug)
print("blocks", len(heights), "merge-mined", merge_mined, "txs", txs_checked, "fees", fees_checked,
      "mismatches", mismatches)
sys.exit(1 if mismatches else 0)
