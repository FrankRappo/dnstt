#!/bin/bash
# Auto-heal SSH tunnels
# Restarts tunnels with: high Send-Q, high RTT, or high retransmit rate
# Runs every 30s via cron on Server1 (203.0.113.178)
#
# Установка: /root/tunnel-cleanup.sh на Server1
# Cron:
#   * * * * * /root/tunnel-cleanup.sh
#   * * * * * sleep 30 && /root/tunnel-cleanup.sh

SENDQ_THRESHOLD=100000   # 100KB
RTT_THRESHOLD=400        # ms (baseline ~49ms)
RETRANS_PCT_THRESHOLD=25 # %
MIN_BYTES=100000         # skip fresh connections

LOG_TAG="tunnel-cleanup"

declare -A PORT_SVC=(
  [10443]="ssh-forward"
  [10444]="ssh-forward-2"
  [10445]="ssh-forward-3"
  [10446]="ssh-forward-4"
  [10447]="ssh-forward-5"
  [10448]="ssh-forward-6"
  [10449]="ssh-forward-7"
  [10450]="ssh-forward-8"
  [10451]="ssh-forward-9"
  [10452]="ssh-forward-10"
)

# Dump all SSH tunnel connections once
SS_DUMP=$(ss -tinp state established "( dport = :56777 )" 2>/dev/null)

for PORT in "${!PORT_SVC[@]}"; do
  SVC="${PORT_SVC[$PORT]}"
  REASON=""

  # Check 0: port not listening at all → immediate restart
  if ! ss -tlnp | grep -q ":${PORT} "; then
    logger -t "$LOG_TAG" "RESTART $SVC (port $PORT): port not listening"
    systemctl restart "$SVC"
    continue
  fi

  MAIN_PID=$(systemctl show -p MainPID "$SVC" --value 2>/dev/null)
  [ "$MAIN_PID" = "0" ] || [ -z "$MAIN_PID" ] && continue
  SSH_PID=$(pgrep -P "$MAIN_PID" -x ssh 2>/dev/null | head -1)
  [ -z "$SSH_PID" ] && continue

  # Extract exactly this PID block (2 lines: connection + tcp info)
  BLOCK=$(echo "$SS_DUMP" | grep -A1 "pid=${SSH_PID}," | head -2)
  [ -z "$BLOCK" ] && continue

  SENDQ=$(echo "$BLOCK" | head -1 | awk "{print \$2}")
  TCP_LINE=$(echo "$BLOCK" | tail -1)

  # Parse RTT
  RTT=$(echo "$TCP_LINE" | grep -oP "(?<=\brtt:)[0-9.]+" | head -1)
  RTT_INT=${RTT%%.*}
  [ -z "$RTT_INT" ] && RTT_INT=0

  # Parse bytes
  BYTES_SENT=$(echo "$TCP_LINE" | grep -oP "(?<=bytes_sent:)[0-9]+" | head -1)
  BYTES_RETRANS=$(echo "$TCP_LINE" | grep -oP "(?<=bytes_retrans:)[0-9]+" | head -1)
  [ -z "$BYTES_SENT" ] && BYTES_SENT=0
  [ -z "$BYTES_RETRANS" ] && BYTES_RETRANS=0
  [ -z "$SENDQ" ] && SENDQ=0

  # Check 1: Send-Q congestion
  if [ "$SENDQ" -gt "$SENDQ_THRESHOLD" ]; then
    REASON="SendQ=${SENDQ}"
  fi

  # Check 2: High RTT (stale TCP state from bad route)
  if [ "$RTT_INT" -gt "$RTT_THRESHOLD" ]; then
    REASON="${REASON:+$REASON, }RTT=${RTT}ms"
  fi

  # Check 3: High retransmit rate
  if [ "$BYTES_SENT" -gt "$MIN_BYTES" ] && [ "$BYTES_RETRANS" -gt 0 ]; then
    RETRANS_PCT=$(( BYTES_RETRANS * 100 / BYTES_SENT ))
    if [ "$RETRANS_PCT" -gt "$RETRANS_PCT_THRESHOLD" ]; then
      REASON="${REASON:+$REASON, }retrans=${RETRANS_PCT}%"
    fi
  fi

  if [ -n "$REASON" ]; then
    logger -t "$LOG_TAG" "RESTART $SVC (port $PORT): $REASON [rtt=${RTT} sent=${BYTES_SENT} retrans=${BYTES_RETRANS} sq=${SENDQ}]"
    systemctl restart "$SVC"
  fi
done
