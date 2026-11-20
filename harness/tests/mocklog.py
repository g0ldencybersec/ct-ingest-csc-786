#!/usr/bin/env python3
"""Synthetic CT log serving BOTH read APIs, for testing ctt without a testbed.

Entries are random bytes, not certificates. Mirrors CompactLog behaviors the
harness must handle: get-entries truncation, Accept-Encoding-driven HTTP
compression, gzip data tiles (optionally double-compressed).

  python3 mocklog.py --port 8099 --entries 1300 [--double-gzip-tiles]
"""
import argparse, base64, gzip, hashlib, json, random, struct
from http.server import ThreadingHTTPServer, BaseHTTPRequestHandler

ap = argparse.ArgumentParser()
ap.add_argument("--port", type=int, default=8099)
ap.add_argument("--entries", type=int, default=1300)
ap.add_argument("--cap", type=int, default=100)
ap.add_argument("--double-gzip-tiles", action="store_true")
args = ap.parse_args()

rng = random.Random(7)
v24 = lambda b: struct.pack(">I", len(b))[1:] + b
v16 = lambda b: struct.pack(">H", len(b)) + b
sha = lambda b: hashlib.sha256(b).digest()
issuers = [bytes(rng.getrandbits(8) for _ in range(rng.randint(800, 1400))) for _ in range(4)]

entries = []  # (leaf_input, extra_data, tile_leaf)
for i in range(args.entries):
    chain = rng.sample(issuers, rng.randint(1, 2))
    ts = struct.pack(">Q", 1_700_000_000_000 + i)
    if rng.random() < 0.25:
        tbs = bytes(rng.getrandbits(8) for _ in range(rng.randint(300, 600)))
        pre = tbs + b"poison"
        te = ts + struct.pack(">H", 1) + sha(chain[0]) + v24(tbs) + v16(b"")
        extra = v24(pre) + v24(b"".join(v24(c) for c in chain))
        tl = te + v24(pre) + v16(b"".join(sha(c) for c in chain))
    else:
        cert = bytes(rng.getrandbits(8) for _ in range(rng.randint(400, 900)))
        te = ts + struct.pack(">H", 0) + v24(cert) + v16(b"")
        extra = v24(b"".join(v24(c) for c in chain))
        tl = te + v16(b"".join(sha(c) for c in chain))
    entries.append((b"\x00\x00" + te, extra, tl))

def mth(hs):
    if len(hs) == 1:
        return hs[0]
    k = 1
    while k * 2 < len(hs):
        k *= 2
    return sha(b"\x01" + mth(hs[:k]) + mth(hs[k:]))

leaves = [sha(b"\x00" + e[0]) for e in entries]
root = base64.b64encode(mth(leaves)).decode()
by_fp = {sha(c).hex(): c for c in issuers}

def tile_path_index(parts):
    n = 0
    for p in parts:
        n = n * 1000 + int(p.lstrip("x"))
    return n

class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *a): pass
    def send(self, body, ctype="application/json", http_gzip_ok=True):
        enc = None
        if http_gzip_ok and "gzip" in self.headers.get("Accept-Encoding", ""):
            body, enc = gzip.compress(body), "gzip"
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        if enc: self.send_header("Content-Encoding", enc)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def do_GET(self):
        p = self.path
        if p == "/health": return self.send(b"ok", "text/plain")
        if p == "/ct/v1/get-sth":
            return self.send(json.dumps({"tree_size": len(entries), "timestamp": 1, "sha256_root_hash": root, "tree_head_signature": ""}).encode())
        if p.startswith("/ct/v1/get-entries"):
            q = dict(kv.split("=") for kv in p.split("?")[1].split("&"))
            s, e = int(q["start"]), min(int(q["end"]), len(entries) - 1, int(q["start"]) + args.cap - 1)
            out = [{"leaf_input": base64.b64encode(l).decode(), "extra_data": base64.b64encode(x).decode()} for l, x, _ in entries[s:e + 1]]
            return self.send(json.dumps({"entries": out}).encode())
        if p.startswith("/tile/data/"):
            parts = p[len("/tile/data/"):].split("/")
            width = 256
            if ".p" in parts[-1] or (len(parts) >= 2 and parts[-2].endswith(".p")):
                width = int(parts[-1]); parts = parts[:-1]; parts[-1] = parts[-1][:-2]
            t = tile_path_index(parts)
            body = gzip.compress(b"".join(e[2] for e in entries[t * 256:t * 256 + width]))
            # Tiles are stored gzipped; optionally let HTTP compression wrap them again.
            return self.send(body, "application/octet-stream", http_gzip_ok=args.double_gzip_tiles)
        if p.startswith("/issuer/"):
            c = by_fp.get(p.split("/")[-1])
            if c is None:
                self.send_response(404); self.send_header("Content-Length", "0"); self.end_headers(); return
            return self.send(c, "application/pkix-cert")
        self.send_response(404); self.send_header("Content-Length", "0"); self.end_headers()

ThreadingHTTPServer(("127.0.0.1", args.port), H).serve_forever()
