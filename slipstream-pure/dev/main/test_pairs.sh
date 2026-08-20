#!/bin/bash
SSH_S="sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no -o ConnectTimeout=5 root@203.0.113.130"
SSH_C="sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no -o ConnectTimeout=5 root@203.0.113.181"

test_pair() {
    echo "--- $1 + $2 ---"
    $SSH_S "killall -9 sp-server sp-server-v15 sp-server.bak 2>/dev/null; true"
    $SSH_C "killall -9 sp-client sp-client-orig sp-client-v16 2>/dev/null; true"
    sleep 2
    $SSH_S "nohup /opt/$3 --domain tunnel.example.com --dns-listen 203.0.113.130:53 >/dev/null 2>&1 &"
    sleep 2
    $SSH_C "nohup $4 --domain tunnel.example.com --config /root/config/resolvers.toml >/dev/null 2>&1 &"
    sleep 8
    for i in 1 2 3 4 5; do
        r=$($SSH_C "curl -s -o /dev/null -w '%{http_code}' --max-time 15 -x socks5h://127.0.0.1:7000 https://httpbin.org/ip")
        echo "  $i: $r"
    done
}

test_pair "v1.5srv" "v1.5cli" "sp-server-v15" "/root/sp-client-orig"
test_pair "v1.5srv" "v1.6cli" "sp-server-v15" "/root/sp-client-v16"
test_pair "v1.6srv" "v1.5cli" "sp-server"     "/root/sp-client-orig"
test_pair "v1.6srv" "v1.6cli" "sp-server"     "/root/sp-client-v16"

echo "=== DONE ==="
