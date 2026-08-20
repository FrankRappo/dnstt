# Diagnostic Results - Section A

## Test Results
- attempt 1: 000 (timeout/fail)
- attempt 2: 200 (success)
- attempt 3: 000 (timeout/fail)

## Server Log Analysis

Key pattern observed:
- Client sends ack=10 repeatedly but server has batch_id=11
- `pending=true` never gets cleared because `client_ack (10) != batch_id (11)`
- The server is stuck resending batch 11 forever, client keeps acking batch 10
- Some polls come with ack=0 (no ack), which also cannot clear pending

## Root Cause
The single-pending-batch design creates a deadlock scenario:
1. Server creates batch 11 and holds it as pending
2. Client only acks batch 10 (the last one it successfully received)
3. Server checks `client_ack == batch_id` (10 == 11 → false), so pending stays
4. Client never receives batch 11 successfully, keeps sending ack=10
5. No new batches can be created while pending exists

This confirms the need for a sliding window approach where:
- Multiple batches can be in-flight
- ACK clears all batches up to the acked ID (not just exact match)
- Retransmission sends the oldest unacked batch
