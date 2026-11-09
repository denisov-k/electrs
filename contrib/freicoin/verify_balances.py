#!/usr/bin/env python3
"""Compares electrs' present values of addresses (their utxos, reference heights and balances
after demurrage) with freicoind's scantxoutset, for addresses paid in random blocks and any given.

    FREICOIND_RPC=http://127.0.0.1:8638 FREICOIND_RPC_AUTH=user:password \
    ESPLORA_URL=http://127.0.0.1:3000 ./verify_balances.py [random addresses] [seed] [address...]

FREICOIND_RPC_AUTH may be left out for FREICOIND_COOKIE, the node's .cookie file.
"""
import base64, json, os, random, sys, urllib.error, urllib.request

NODE = os.environ.get("FREICOIND_RPC", "http://127.0.0.1:8638")
ESPLORA = os.environ.get("ESPLORA_URL", "http://127.0.0.1:3000").rstrip("/")
AUTH = os.environ.get("FREICOIND_RPC_AUTH") or open(os.environ["FREICOIND_COOKIE"]).read().strip()
HEADERS = {"Authorization": "Basic " + base64.b64encode(AUTH.encode()).decode()}


def rpc(method, *params):
    body = json.dumps({"method": method, "params": list(params), "id": 1}).encode()
    request = urllib.request.Request(NODE, body, HEADERS)
    return json.load(urllib.request.urlopen(request, timeout=600))["result"]


def rest(path):
    try:
        return json.loads(urllib.request.urlopen(ESPLORA + path).read())
    except urllib.error.HTTPError as e:
        return {"error": e.code, "body": e.read()[:200].decode()}


def kria(amount):
    return round(amount * 100000000)


random.seed(int(sys.argv[2]) if len(sys.argv) > 2 else 1)
count = int(sys.argv[1]) if len(sys.argv) > 1 else 20
tip = rpc("getblockcount")
addresses = list(sys.argv[3:])
while len(addresses) < count + len(sys.argv[3:]):
    height = random.randrange(tip - 20000, tip) if random.random() < 0.7 else random.randrange(tip)
    blockhash = rpc("getblockhash", height)
    for txid in rpc("getblock", blockhash, 1)["tx"][:3]:
        for out in rpc("getrawtransaction", txid, 1, blockhash)["vout"]:
            address = out["scriptPubKey"].get("address")
            if address and address not in addresses:
                addresses.append(address)
                break

mismatches = 0; checked = 0; utxos = 0
for address in addresses:
    height = rpc("getblockcount")
    scan = rpc("scantxoutset", "start", ["addr(%s)" % address])
    eu = rest("/address/%s/utxo" % address)
    ea = rest("/address/%s" % address)
    if rpc("getblockcount") != height or scan["height"] != height:
        print("the tip moved, skipping", address)
        continue
    if isinstance(eu, dict):
        print("electrs:", address, eu, "node utxos", len(scan["unspents"]))
        continue
    checked += 1
    node = {(u["txid"], u["vout"]): (u["refheight"], kria(u["amount"])) for u in scan["unspents"]}
    mine = {(u["txid"], u["vout"]): (u["refheight"], u["present_value"])
            for u in eu if u["status"]["confirmed"]}
    utxos += len(node)
    present = ea.get("present_value")
    problems = []
    if node != mine:
        problems.append("utxos differ: node %s electrs %s" % (
            {k: v for k, v in node.items() if mine.get(k) != v},
            {k: v for k, v in mine.items() if node.get(k) != v}))
    if (present is None or present["height"] != height + 1
            or present["confirmed"] != kria(scan["total_amount"])):
        problems.append("balance node %s electrs %s" % (kria(scan["total_amount"]), present))
    if problems:
        mismatches += 1
        print("MISMATCH", address, problems)
    else:
        print("ok", address, len(node), "utxos, balance", present["confirmed"])
print("addresses", checked, "utxos", utxos, "mismatches", mismatches)
sys.exit(1 if mismatches else 0)
