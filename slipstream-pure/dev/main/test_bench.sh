#!/bin/bash
# Benchmark all server+client version pairs
# Each pair: same version server + client
# Measures: HTTPS success rate + download speed

SERVER_IP=203.0.113.130
SERVER_PASS='<REDACTED_PASSWORD>'
CLIENT_IP=203.0.113.181
CLIENT_PASS='<REDACTED_PASSWORD>'

SSH_S="sshpass -p $SERVER_PASS ssh -o StrictHostKeyChecking=no -o ConnectTimeout=5 root@$SERVER_IP"
SSH_C="sshpass -p $CLIENT_PASS ssh -o StrictHostKeyChecking=no -o ConnectTimeout=5 root@$CLIENT_IP"

RESULTS_FILE="/work/vpn/slipstream-pure-dev/benchmark_results.txt"
echo "=== Benchmark $(date) ===" > $RESULTS_FILE

test_version() {
    local name=$1
    local srv_bin=$2
    local cli_bin=$3

    echo "" >> $RESULTS_FILE
    echo "--- $name ---" >> $RESULTS_FILE
    echo "--- $name ---"

    # Kill everything
    $SSH_S "killall -9 sp-server sp-server-v15 sp-server-v10 sp-server.bak sp-server-v15stable 2>/dev/null; true"
    $SSH_C "killall -9 sp-client sp-client-orig sp-client-v16 sp-client-v10 sp-client-v15stable 2>/dev/null; true"
    sleep 2

    # Start server
    $SSH_S "nohup $srv_bin >/dev/null 2>&1 &"
    sleep 2

    # Start client
    $SSH_C "nohup $cli_bin >/dev/null 2>&1 &"
    sleep 8

    # HTTPS test (5 requests)
    echo "  HTTPS:" >> $RESULTS_FILE
    local ok=0
    for i in 1 2 3 4 5; do
        r=$($SSH_C "curl -s -o /dev/null -w '%{http_code}' --max-time 15 -x socks5h://127.0.0.1:7000 https://httpbin.org/ip")
        echo "  $i: $r" >> $RESULTS_FILE
        echo "  $i: $r"
        if [ "$r" = "'200'" ] || [ "$r" = "200" ]; then
            ok=$((ok+1))
        fi
    done
    echo "  HTTPS: $ok/5" >> $RESULTS_FILE

    # Speed test (download 100KB file via HTTP, measure time)
    echo "  Speed:" >> $RESULTS_FILE
    for i in 1 2 3; do
        speed=$($SSH_C "curl -s -w '%{speed_download}' -o /dev/null --max-time 30 -x socks5h://127.0.0.1:7000 http://httpbin.org/bytes/102400")
        echo "  speed $i: $speed B/s" >> $RESULTS_FILE
        echo "  speed $i: $speed B/s"
    done

    echo "" >> $RESULTS_FILE
}

# Build all versions first
echo "Building versions..." >> $RESULTS_FILE

# v1.0 (FINAL-1115)
if [ ! -f /tmp/backups/slipstream-pure-FINAL-20260324-1115/crates/sp-server/src/main.rs ]; then
    mkdir -p /tmp/backups/slipstream-pure-FINAL-20260324-1115
    tar xzf /work/vpn/slipstream-pure-FINAL-20260324-1115.tar.gz -C /tmp/backups/slipstream-pure-FINAL-20260324-1115 --strip-components=1 2>/dev/null
fi
cd /tmp/backups/slipstream-pure-FINAL-20260324-1115
CARGO_TARGET_DIR=/tmp/build-v10 cargo build --release 2>&1 | tail -1
echo "v1.0 built" >> $RESULTS_FILE

# v1.5-9KBs-STABLE
if [ ! -f /tmp/backups/slipstream-pure-v1.5-9KBs-STABLE-20260324-1428/crates/sp-server/src/main.rs ]; then
    mkdir -p /tmp/backups/slipstream-pure-v1.5-9KBs-STABLE-20260324-1428
    tar xzf /work/vpn/slipstream-pure-v1.5-9KBs-STABLE-20260324-1428.tar.gz -C /tmp/backups/slipstream-pure-v1.5-9KBs-STABLE-20260324-1428 --strip-components=1 2>/dev/null
fi
cd /tmp/backups/slipstream-pure-v1.5-9KBs-STABLE-20260324-1428
CARGO_TARGET_DIR=/tmp/build-v15stable cargo build --release 2>&1 | tail -1
echo "v1.5-stable built" >> $RESULTS_FILE

# Upload all binaries to servers
echo "Uploading binaries..." >> $RESULTS_FILE

# v1.0
sshpass -p $SERVER_PASS scp -o StrictHostKeyChecking=no /tmp/build-v10/release/sp-server root@$SERVER_IP:/opt/sp-server-v10
sshpass -p $CLIENT_PASS scp -o StrictHostKeyChecking=no /tmp/build-v10/release/sp-client root@$CLIENT_IP:/root/sp-client-v10
$SSH_S "chmod +x /opt/sp-server-v10"
$SSH_C "chmod +x /root/sp-client-v10"

# v1.5-stable
sshpass -p $SERVER_PASS scp -o StrictHostKeyChecking=no /tmp/build-v15stable/release/sp-server root@$SERVER_IP:/opt/sp-server-v15stable
sshpass -p $CLIENT_PASS scp -o StrictHostKeyChecking=no /tmp/build-v15stable/release/sp-client root@$CLIENT_IP:/root/sp-client-v15stable
$SSH_S "chmod +x /opt/sp-server-v15stable"
$SSH_C "chmod +x /root/sp-client-v15stable"

echo "All uploaded" >> $RESULTS_FILE

# Test each version pair
# v1.0 final
test_version "v1.0-FINAL" \
    "/opt/sp-server-v10 --domain tunnel.example.com --dns-listen 203.0.113.130:53" \
    "/root/sp-client-v10 --domain tunnel.example.com --config /root/config/resolvers.toml"

# v1.5 9KBs stable (the one documented as working)
test_version "v1.5-9KBs-STABLE" \
    "/opt/sp-server-v15stable --domain tunnel.example.com --dns-listen 203.0.113.130:53" \
    "/root/sp-client-v15stable --domain tunnel.example.com --config /root/config/resolvers.toml"

# v1.5 original (from binaries/ dir, what was deployed on 24 march)
test_version "v1.5-DEPLOYED" \
    "/opt/sp-server-v15 --domain tunnel.example.com --dns-listen 203.0.113.130:53" \
    "/root/sp-client-orig --domain tunnel.example.com --config /root/config/resolvers.toml"

# v1.6 sliding window (our new code)
test_version "v1.6-WINDOW" \
    "/opt/sp-server --domain tunnel.example.com --dns-listen 203.0.113.130:53" \
    "/root/sp-client-v16 --domain tunnel.example.com --config /root/config/resolvers.toml"

echo "" >> $RESULTS_FILE
echo "=== DONE $(date) ===" >> $RESULTS_FILE
echo "=== DONE ==="

cat $RESULTS_FILE
