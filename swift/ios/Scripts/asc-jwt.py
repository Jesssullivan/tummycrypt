#!/usr/bin/env python3
"""Generate JWT for App Store Connect API."""
import json, time, base64, hashlib, sys

key_id = sys.argv[1]
issuer_id = sys.argv[2]
key_file = sys.argv[3]

# Read the P8 key
with open(key_file, 'r') as f:
    key_data = f.read()

# JWT header
header = {"alg": "ES256", "kid": key_id, "typ": "JWT"}

# JWT payload
now = int(time.time())
payload = {
    "iss": issuer_id,
    "iat": now,
    "exp": now + 1200,
    "aud": "appstoreconnect-v1"
}

def b64url(data):
    return base64.urlsafe_b64encode(data).rstrip(b'=').decode()

header_b64 = b64url(json.dumps(header).encode())
payload_b64 = b64url(json.dumps(payload).encode())
message = f"{header_b64}.{payload_b64}"

# Sign with ES256
try:
    from cryptography.hazmat.primitives.asymmetric import ec, utils
    from cryptography.hazmat.primitives import hashes, serialization
    private_key = serialization.load_pem_private_key(key_data.encode(), password=None)
    der_sig = private_key.sign(message.encode(), ec.ECDSA(hashes.SHA256()))
    # Convert DER to raw r||s
    r, s = utils.decode_dss_signature(der_sig)
    sig = r.to_bytes(32, 'big') + s.to_bytes(32, 'big')
    print(f"{message}.{b64url(sig)}")
except ImportError:
    # Fallback: use openssl
    import subprocess, tempfile, os
    with tempfile.NamedTemporaryFile(mode='w', suffix='.pem', delete=False) as kf:
        kf.write(key_data)
        kf_path = kf.name
    with tempfile.NamedTemporaryFile(mode='w', suffix='.txt', delete=False) as mf:
        mf.write(message)
        mf_path = mf.name
    result = subprocess.run(
        ['openssl', 'dgst', '-sha256', '-sign', kf_path, mf_path],
        capture_output=True
    )
    os.unlink(kf_path)
    os.unlink(mf_path)
    # DER sig to raw r||s
    der = result.stdout
    # Parse DER SEQUENCE
    assert der[0] == 0x30
    seq_len = der[1]
    pos = 2
    assert der[pos] == 0x02
    r_len = der[pos+1]
    r_bytes = der[pos+2:pos+2+r_len]
    pos = pos + 2 + r_len
    assert der[pos] == 0x02
    s_len = der[pos+1]
    s_bytes = der[pos+2:pos+2+s_len]
    r = int.from_bytes(r_bytes, 'big')
    s = int.from_bytes(s_bytes, 'big')
    sig = r.to_bytes(32, 'big') + s.to_bytes(32, 'big')
    print(f"{message}.{b64url(sig)}")
