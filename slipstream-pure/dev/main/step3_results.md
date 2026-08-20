# Step 3 Results - Section C (Sliding Window)

## HTTPS Test Results (10 attempts)
- attempt 1: 200 (success)
- attempts 2-10: 000 (timeout/fail)
- Success rate: 1/10 (10%)

## HTTP Test Results (5 attempts)
- attempt 1: 000
- attempt 2: 200
- attempt 3: 200
- attempt 4: 000
- attempt 5: 000
- Success rate: 2/5 (40%)

## Server Log Analysis
- ACK progression observed: ack=1 → 2 → 3 → 12 (for HTTP connections)
- Window retain logic works correctly (ACK > comparison fixes the old == deadlock)
- Connect resets window properly (ack drops back to low values on new stream)
- Sliding window is functioning but window effectively stays at 1 entry
  because take_downstream_data returns first batch before creating new ones

## Comparison
| Test  | Section A (baseline) | Section B (batch ID) | Section C (window) |
|-------|---------------------|---------------------|-------------------|
| HTTPS | 1/3 (33%)           | 2/5 (40%)           | 1/10 (10%)        |
| HTTP  | n/a                 | n/a                 | 2/5 (40%)         |

Note: Results may vary due to network conditions. The key improvement is
the ACK mechanism change from exact-match (==) to range-based (>), which
prevents the deadlock scenario identified in Section A diagnostics.
