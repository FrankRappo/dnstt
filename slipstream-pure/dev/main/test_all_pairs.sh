#!/bin/bash
# Test all server+client pairs
# Servers: v1.5 original, v1.6 sliding window
# Clients: v1.5 original (sp-client-orig), v1.6 with BatchTracker (sp-client)
# v1.6 server + v1.5 client incompatible (batch_id prefix) but test anyway

SERVER_IP=203.0.113.130
SERVER_PASS='<REDACTED_PASSWORD>'
CLIENT_IP=203.0.113.181
CLIENT_PASS='<REDACTED_PASSWORD>'

SSH_S="sshpass -p $SERVER_PASS ssh -o StrictHostKeyChecking=no root@$SERVER_IP"
SSH_C="sshpass -p $CLIENT_PASS ssh -o StrictHostKeyChecking=no root@$CLIENT_IP"
SCP_C="sshpass -p $CLIENT_PASS scp -o StrictHostKeyChecking=no"

# Upload v1.6 client if not there
$SCP_C /work/vpn/slipstream-pure-dev/target/release/sp-client root@$CLIENT_IP:/root/sp-client-v16
$SSH_C 'chmod +x /root/sp-client-v16'

RESULTS=""

run_test() {
    local server_bin=$1
    local server_name=$2
    local client_bin=$3
    local client_name=$4

    echo "=== $server_name + $client_name ==="

    # Kill everything
    $SSH_S "killall -9 sp-server sp-server-v15 sp-server.bak 2>/dev/null"
    $SSH_C "killall -9 sp-client sp-client-orig sp-client-v16 2>/dev/null"
    sleep 2

    # Start server
    $SSH_S "cd /opt && nohup $server_bin --domain tunnel.example.com --dns-listen 203.0.113.130:53 > /dev/null 2>&1 &"
    sleep 2

    # Start client
    $SSH_C "RUST_LOG=warn nohup $client_bin --domain tunnel.example.com --config /root/config/resolvers.toml > /dev/null 2>&1 &"
    sleep 8

    # Test
    local result=$($SSH_C 'for i in $(seq 1 5); do curl -s -o /dev/null -w "%{http_code} " --max-time 20 -x socks5h://127.0.0.1:7000 https://httpbin.org/ip; done')
    local ok=$(echo "$result" | grep -o "200" | wc -l)
    echo "  HTTPS: $result  ($ok/5)"
    RESULTS="$RESULTS\n$server_name + $client_name: $ok/5 [$result]"
}

# Pair 1: v1.5 server + v1.5 client
run_test "./sp-server-v15" "v1.5-server" "/root/sp-client-orig" "v1.5-client"

# Pair 2: v1.5 server + v1.6 client
run_test "./sp-server-v15" "v1.5-server" "/root/sp-client-v16" "v1.6-client"

# Pair 3: v1.6 server + v1.5 client (incompatible but test)
run_test "./sp-server" "v1.6-server" "/root/sp-client-orig" "v1.5-client"

# Pair 4: v1.6 server + v1.6 client (optimal pair)
run_test "./sp-server" "v1.6-server" "/root/sp-client-v16" "v1.6-client"

echo ""
echo "========== RESULTS =========="
echo -e "$RESULTS"
