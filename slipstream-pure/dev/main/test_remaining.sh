#!/bin/bash
SSH_S="sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no -o ConnectTimeout=5 root@203.0.113.130"
SSH_C="sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no -o ConnectTimeout=5 root@203.0.113.181"

test_pair() {
    local name=$1 srv=$2 cli=$3
    echo "--- $name ---"
    $SSH_S "killall -9 sp-server sp-server-v10 sp-server-v10early sp-server-v15 sp-server-v15stable sp-server-v16p sp-server.bak 2>/dev/null; true"
    $SSH_C "killall -9 sp-client sp-client-orig sp-client-v10 sp-client-v10early sp-client-v15pre sp-client-v15cur sp-client-v16 sp-client-v16p sp-client-v15stable 2>/dev/null; true"
    sleep 2
    $SSH_S "nohup $srv >/dev/null 2>&1 &"
    sleep 2
    $SSH_C "nohup $cli >/dev/null 2>&1 &"
    sleep 8
    echo "  HTTPS:"
    for i in 1 2 3 4 5; do
        r=$($SSH_C "curl -s -o /dev/null -w '%{http_code}' --max-time 15 -x socks5h://127.0.0.1:7000 https://httpbin.org/ip")
        echo "    $i: $r"
    done
    echo "  HTTP:"
    for i in 1 2 3; do
        r=$($SSH_C "curl -s -o /dev/null -w '%{http_code}' --max-time 15 -x socks5h://127.0.0.1:7000 http://httpbin.org/ip")
        echo "    $i: $r"
    done
    echo "  Speed:"
    for i in 1 2; do
        s=$($SSH_C "curl -s -w '%{speed_download}' -o /dev/null --max-time 30 -x socks5h://127.0.0.1:7000 http://httpbin.org/bytes/102400")
        echo "    $i: $s B/s"
    done
}

# 1. WORKING-0901 (v1.0 early)
test_pair "v1.0-EARLY(0901)" \
    "/opt/sp-server-v10early --domain tunnel.example.com --dns-listen 203.0.113.130:53" \
    "/root/sp-client-v10early --domain tunnel.example.com --config /root/config/resolvers.toml"

# 2. v1.5-pre-retry
test_pair "v1.5-PRE-RETRY" \
    "/opt/sp-server-v15stable --domain tunnel.example.com --dns-listen 203.0.113.130:53" \
    "/root/sp-client-v15pre --domain tunnel.example.com --config /root/config/resolvers.toml"

# 3. v1.5-CURRENT (with retry)
test_pair "v1.5-CURRENT" \
    "/opt/sp-server-v15stable --domain tunnel.example.com --dns-listen 203.0.113.130:53" \
    "/root/sp-client-v15cur --domain tunnel.example.com --config /root/config/resolvers.toml"

# 4. Re-test previous winners with HTTP too
test_pair "v1.0-FINAL(retest)" \
    "/opt/sp-server-v10 --domain tunnel.example.com --dns-listen 203.0.113.130:53" \
    "/root/sp-client-v10 --domain tunnel.example.com --config /root/config/resolvers.toml"

test_pair "v1.5-STABLE(retest)" \
    "/opt/sp-server-v15stable --domain tunnel.example.com --dns-listen 203.0.113.130:53" \
    "/root/sp-client-v15stable --domain tunnel.example.com --config /root/config/resolvers.toml"

test_pair "v1.6-WINDOW(retest)" \
    "/opt/sp-server --domain tunnel.example.com --dns-listen 203.0.113.130:53" \
    "/root/sp-client-v16 --domain tunnel.example.com --config /root/config/resolvers.toml"

echo "=== DONE ==="
