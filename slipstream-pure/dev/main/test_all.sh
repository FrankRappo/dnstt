#!/bin/bash
SSH_S="sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no -o ConnectTimeout=5 root@203.0.113.130"
SSH_C="sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no -o ConnectTimeout=5 root@203.0.113.181"

test_pair() {
    local sname=$1 sbin=$2 cname=$3 cbin=$4
    echo "--- $sname + $cname ---"
    $SSH_S "killall -9 sp-server sp-server-v15 sp-server.bak 2>/dev/null; true"
    $SSH_C "killall -9 sp-client sp-client-orig sp-client-v16 2>/dev/null; true"
    sleep 2
    $SSH_S "cd /opt && $sbin --domain tunnel.example.com --dns-listen 203.0.113.130:53 &"
    sleep 2
    $SSH_C "RUST_LOG=warn $cbin --domain tunnel.example.com --config /root/config/resolvers.toml &"
    sleep 8
    for i in 1 2 3 4 5; do
        r=$($SSH_C "curl -s -o /dev/null -w '%{http_code}' --max-time 15 -x socks5h://127.0.0.1:7000 https://httpbin.org/ip")
        echo "  $i: $r"
    done
}

test_pair "v1.5srv" "./sp-server-v15" "v1.5cli" "/root/sp-client-orig"
test_pair "v1.5srv" "./sp-server-v15" "v1.6cli" "/root/sp-client-v16"
test_pair "v1.6srv" "./sp-server"     "v1.5cli" "/root/sp-client-orig"
test_pair "v1.6srv" "./sp-server"     "v1.6cli" "/root/sp-client-v16"

echo "=== DONE ==="
