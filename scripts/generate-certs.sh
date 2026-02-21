#!/bin/bash
# scripts/generate-certs.sh
# Generate SeaweedFS TLS certificates

CERT_DIR="./certs"
CA_CERT="${CERT_DIR}/SeaweedFS_CA.crt"
CA_KEY="${CERT_DIR}/SeaweedFS_CA.key"

mkdir -p "$CERT_DIR"

# Generate CA if not exists
if [ ! -f "$CA_CERT" ]; then
    echo "Generating CA certificate..."
    openssl genrsa -out "$CA_KEY" 2048
    openssl req -new -x509 -days 3650 \
        -key "$CA_KEY" \
        -out "$CA_CERT" \
        -subj "/CN=SeaweedFS CA"
    echo "✓ CA generated at $CA_CERT"
fi

# Generate certificates for each component
COMPONENTS="master-1 master-2 master-3 volume filer s3"
for component in $COMPONENTS; do
    KEY="${CERT_DIR}/${component}.key"
    CSR="${CERT_DIR}/${component}.csr"
    CRT="${CERT_DIR}/${component}.crt"

    if [ -f "$CRT" ]; then
        echo "✓ $component certificate already exists"
        continue
    fi

    echo "Generating $component certificate..."
    openssl genrsa -out "$KEY" 2048
    openssl req -new -key "$KEY" -out "$CSR" \
        -subj "/CN=${component}"
    openssl x509 -req -days 3650 -in "$CSR" \
        -CA "$CA_CERT" -CAkey "$CA_KEY" -CAcreateserial \
        -out "$CRT"
    rm "$CSR"
    echo "✓ $component certificate generated"
done

echo ""
echo "=== Certificate Summary ==="
echo "Directory: $CERT_DIR"
echo "CA Certificate: $CA_CERT"
echo ""
ls -lh "$CERT_DIR"
