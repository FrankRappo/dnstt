# Задача: Sliding Window для downstream delivery

Выполняй СТРОГО ПО ПОРЯДКУ: секция A → B → C. Не забегай вперёд.

---

## СЕКЦИЯ A: Диагностика

### A1. Добавь логи на сервер

Файл: `crates/sp-server/src/main.rs`

Найди функцию `take_downstream_data` (~строка 454). В начале, после получения locks, добавь:

```rust
    let queue_len = session.downstream_queue.lock().await.len();
    debug!("downstream: ack={}, batch_id={}, pending={}, queue={}", client_ack, *batch_id, pending.is_some(), queue_len);
```

Найди `PacketType::Poll => {}` (~строка 351). Замени на:
```rust
PacketType::Poll => {
    debug!("poll: session={}, ack_payload={:?}", session.id, &packet.payload[..]);
}
```

### A2. Собери и задеплой ТОЛЬКО сервер

```bash
cd /work/vpn/slipstream-pure-dev && cargo build --release 2>&1
```

```bash
sshpass -p '<REDACTED_PASSWORD>' scp -o StrictHostKeyChecking=no target/release/sp-server root@203.0.113.130:/opt/sp-server-new
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.130 'cd /opt && cp sp-server sp-server.bak && pkill sp-server; sleep 1; mv sp-server-new sp-server; chmod +x sp-server; RUST_LOG=info,sp_server=debug nohup ./sp-server --domain tunnel.example.com --dns-listen 203.0.113.130:53 > /var/log/sp-server.log 2>&1 &'
sleep 3
```

Перезапусти клиент (бинарник `/root/sp-client-orig` уже на сервере):
```bash
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.181 'pkill -9 sp-client; sleep 2; rm -f /var/log/sp-client.log; RUST_LOG=info,sp_client=debug nohup /root/sp-client-orig --domain tunnel.example.com --config /root/config/resolvers.toml > /var/log/sp-client.log 2>&1 &'
sleep 8
```

### A3. Тест и сбор логов

```bash
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.181 'for i in $(seq 1 3); do curl -s -o /dev/null -w "%{http_code}" --max-time 35 -x socks5h://127.0.0.1:7000 https://httpbin.org/ip; echo " attempt $i"; done'
```

Серверные логи:
```bash
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.130 'grep -E "downstream:|poll:" /var/log/sp-server.log | tail -30'
```

Запиши результаты (что видно в логах) в `/work/vpn/slipstream-pure-dev/diag_results.md`.

**После записи результатов — переходи к СЕКЦИИ B.**

---

## СЕКЦИЯ B: Batch ID prefix + BatchTracker

### B1. Сервер: batch_id как 4-байтный префикс

Файл: `crates/sp-server/src/main.rs`

Найди в `take_downstream_data` блок (~строка 488-496):
```rust
    *batch_id = batch_id.wrapping_add(1);

    let mut combined = Vec::with_capacity(total);
    for chunk in taken {
        combined.extend_from_slice(&chunk);
    }
```

Замени на:
```rust
    *batch_id = batch_id.wrapping_add(1);

    let mut combined = Vec::with_capacity(4 + total);
    combined.extend_from_slice(&batch_id.to_be_bytes());
    for chunk in taken {
        combined.extend_from_slice(&chunk);
    }
```

### B2. Клиент: добавить BatchTracker

Файл: `crates/sp-client/src/main.rs`

Найди struct `DsHashRing`. Замени ВСЮ структуру и impl на:

```rust
struct BatchTracker {
    received: std::collections::BTreeSet<u32>,
    ack: u32,
}

impl BatchTracker {
    fn new() -> Self {
        Self { received: std::collections::BTreeSet::new(), ack: 0 }
    }

    fn receive(&mut self, batch_id: u32) -> bool {
        if batch_id == 0 || batch_id <= self.ack { return false; }
        let is_new = self.received.insert(batch_id);
        while self.received.contains(&(self.ack + 1)) {
            self.ack += 1;
            self.received.remove(&self.ack);
        }
        is_new
    }
}
```

### B3. Клиент: новая dispatch функция

Найди функцию `dispatch_downstream_inline`. Замени ВСЮ функцию на:

```rust
async fn dispatch_downstream_with_batch(
    txt_data: &[u8],
    streams: &Arc<RwLock<HashMap<u16, StreamDownstream>>>,
    tracker: &tokio::sync::Mutex<BatchTracker>,
) -> u32 {
    let txt_str = match std::str::from_utf8(txt_data) {
        Ok(s) => s,
        Err(_) => return 0,
    };
    let decoded = match dns_codec::decode_downstream(txt_str) {
        Ok(d) => d,
        Err(_) => return 0,
    };
    if decoded.is_empty() { return 0; }
    let decompressed = compress::decompress(&decoded).unwrap_or(decoded);
    if decompressed.len() < 4 { return 0; }

    let server_batch_id = u32::from_be_bytes([
        decompressed[0], decompressed[1], decompressed[2], decompressed[3],
    ]);
    let packet_data = &decompressed[4..];

    {
        let mut t = tracker.lock().await;
        if !t.receive(server_batch_id) {
            return 0;
        }
    }

    let packets = TunnelPacket::decode_all(packet_data);
    let mut streams = streams.write().await;
    for packet in packets {
        match packet.packet_type {
            PacketType::Data => {
                if let Some(state) = streams.get_mut(&packet.stream_id) {
                    dispatch_ds_packet(state, packet.seq_num, packet.payload.to_vec()).await;
                }
            }
            PacketType::Disconnect => {
                if let Some(state) = streams.get(&packet.stream_id) {
                    let _ = state.tx.send(Vec::new()).await;
                }
            }
            _ => {}
        }
    }
    server_batch_id
}
```

### B4. Клиент: обновить pump

В retry loop, найди где создаётся `ds_hashes` и замени:

```rust
// БЫЛО:  let ds_hashes = Arc::new(DsHashRing::new());
// СТАЛО:
let batch_tracker = Arc::new(tokio::sync::Mutex::new(BatchTracker::new()));
```

В pump spawn, замени clone:
```rust
// БЫЛО:  let ds_hashes = ds_hashes.clone();
// СТАЛО:
let batch_tracker = batch_tracker.clone();
```

В pump, замени dispatch вызов:
```rust
// БЫЛО:
//  let is_new = dispatch_downstream_inline(
//      resp.txt_data.as_ref().unwrap(),
//      &streams,
//      &*ds_hashes,
//  ).await;
//  ...
//  if is_new {
//      batch_id.fetch_add(1, Ordering::Relaxed);
//  }

// СТАЛО:
let recv_id = dispatch_downstream_with_batch(
    resp.txt_data.as_ref().unwrap(),
    &streams,
    &batch_tracker,
).await;
if recv_id > 0 {
    router.report_downstream_success(pid).await;
    let t = batch_tracker.lock().await;
    batch_id.store(t.ack, Ordering::Relaxed);
}
```

### B5. Клиент: убрать неиспользуемое

- Удали struct `DsHashRing` и весь её impl (если ещё не заменил в B2)
- Найди все `_final_ds_hashes` — замени на `_final_batch_tracker` с типом `Option<Arc<tokio::sync::Mutex<BatchTracker>>>`, инициализируй как `None`
- В success branch: `_final_batch_tracker = Some(batch_tracker);`
- Поищи другие упоминания `ds_hash` — исправь или удали

### B6. Собери

```bash
cd /work/vpn/slipstream-pure-dev && cargo build --release 2>&1
```

Исправь ВСЕ ошибки компиляции. Warnings допустимы.

### B7. Задеплой ОБОИХ и протестируй

Сервер:
```bash
sshpass -p '<REDACTED_PASSWORD>' scp -o StrictHostKeyChecking=no target/release/sp-server root@203.0.113.130:/opt/sp-server-new
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.130 'cd /opt && cp sp-server sp-server.bak && pkill sp-server; sleep 1; mv sp-server-new sp-server; chmod +x sp-server; RUST_LOG=info,sp_server=debug nohup ./sp-server --domain tunnel.example.com --dns-listen 203.0.113.130:53 > /var/log/sp-server.log 2>&1 &'
sleep 3
```

Клиент:
```bash
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.181 'pkill -9 sp-client; sleep 2; rm -f /root/sp-client /var/log/sp-client.log'
sshpass -p '<REDACTED_PASSWORD>' scp -o StrictHostKeyChecking=no target/release/sp-client root@203.0.113.181:/root/
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.181 'chmod +x /root/sp-client && RUST_LOG=info,sp_client=debug nohup /root/sp-client --domain tunnel.example.com --config /root/config/resolvers.toml > /var/log/sp-client.log 2>&1 &'
sleep 8
```

Тест:
```bash
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.181 'for i in $(seq 1 5); do curl -s -o /dev/null -w "%{http_code}" --max-time 35 -x socks5h://127.0.0.1:7000 https://httpbin.org/ip; echo " attempt $i"; done'
```

Запиши результат в `/work/vpn/slipstream-pure-dev/step2_results.md`.

**После записи — переходи к СЕКЦИИ C.**

---

## СЕКЦИЯ C: Sliding Window

### C1. Прочитай текущий код сервера

```bash
cat crates/sp-server/src/main.rs
```

Убедись что batch_id prefix из секции B работает.

### C2. Новая структура DownstreamWindow

Файл: `crates/sp-server/src/main.rs`

Добавь ПЕРЕД struct Session:

```rust
struct DownstreamWindow {
    batches: Vec<(u32, Vec<u8>)>,
    next_batch_id: u32,
}

impl DownstreamWindow {
    fn new() -> Self {
        Self { batches: Vec::new(), next_batch_id: 0 }
    }
}
```

### C3. Замени поля Session

В struct Session замени:
```rust
// УДАЛИ эти два поля:
//  pending_downstream: tokio::sync::Mutex<Option<Vec<u8>>>,
//  ds_batch_id: tokio::sync::Mutex<u32>,

// ДОБАВЬ:
    window: tokio::sync::Mutex<DownstreamWindow>,
```

### C4. Обнови создание Session

В `get_or_create_session`, замени поля:
```rust
// УДАЛИ:
//  pending_downstream: tokio::sync::Mutex::new(None),
//  ds_batch_id: tokio::sync::Mutex::new(0),

// ДОБАВЬ:
        window: tokio::sync::Mutex::new(DownstreamWindow::new()),
```

### C5. Обнови Connect handler

В `handle_tunnel_packet`, PacketType::Connect, замени:
```rust
// БЫЛО:
//  *session.pending_downstream.lock().await = None;
//  *session.ds_batch_id.lock().await = 0;
//  session.downstream_queue.lock().await.clear();

// СТАЛО:
*session.window.lock().await = DownstreamWindow::new();
session.downstream_queue.lock().await.clear();
```

### C6. Замени take_downstream_data ЦЕЛИКОМ

```rust
async fn take_downstream_data(session: &Session, client_ack: u32) -> Option<Vec<u8>> {
    const MAX_RAW: usize = 500;
    const WINDOW_SIZE: usize = 4;

    let mut win = session.window.lock().await;

    if client_ack > 0 {
        win.batches.retain(|(id, _)| *id > client_ack);
    }

    if let Some((_id, data)) = win.batches.first() {
        return Some(data.clone());
    }

    if win.batches.len() >= WINDOW_SIZE {
        return None;
    }

    let mut queue = session.downstream_queue.lock().await;
    if queue.is_empty() {
        return None;
    }

    let mut total = 0;
    let mut taken = Vec::new();
    while let Some(data) = queue.first() {
        if total > 0 && total + data.len() > MAX_RAW { break; }
        total += data.len();
        taken.push(queue.remove(0));
    }
    if taken.is_empty() { return None; }

    win.next_batch_id += 1;
    let bid = win.next_batch_id;

    let mut combined = Vec::with_capacity(4 + total);
    combined.extend_from_slice(&bid.to_be_bytes());
    for chunk in taken {
        combined.extend_from_slice(&chunk);
    }

    win.batches.push((bid, combined.clone()));
    Some(combined)
}
```

### C7. Исправь ошибки компиляции

Поищи оставшиеся ссылки на `pending_downstream` и `ds_batch_id`:
```bash
grep -n "pending_downstream\|ds_batch_id" crates/sp-server/src/main.rs
```
Удали или замени все найденные.

### C8. Собери

```bash
cd /work/vpn/slipstream-pure-dev && cargo build --release 2>&1
```

### C9. Задеплой ТОЛЬКО сервер и протестируй

Сервер:
```bash
sshpass -p '<REDACTED_PASSWORD>' scp -o StrictHostKeyChecking=no target/release/sp-server root@203.0.113.130:/opt/sp-server-new
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.130 'cd /opt && cp sp-server sp-server.bak && pkill sp-server; sleep 1; mv sp-server-new sp-server; chmod +x sp-server; RUST_LOG=info,sp_server=debug nohup ./sp-server --domain tunnel.example.com --dns-listen 203.0.113.130:53 > /var/log/sp-server.log 2>&1 &'
sleep 3
```

Перезапусти клиент (бинарник из секции B, НЕ пересобирать):
```bash
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.181 'pkill -9 sp-client; sleep 2; rm -f /var/log/sp-client.log; RUST_LOG=info,sp_client=debug nohup /root/sp-client --domain tunnel.example.com --config /root/config/resolvers.toml > /var/log/sp-client.log 2>&1 &'
sleep 8
```

Тест (10 запросов):
```bash
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.181 'for i in $(seq 1 10); do curl -s -o /dev/null -w "%{http_code}" --max-time 35 -x socks5h://127.0.0.1:7000 https://httpbin.org/ip; echo " attempt $i"; done'
```

Запиши результат в `/work/vpn/slipstream-pure-dev/step3_results.md`.

Также проверь HTTP (должен быть 100%):
```bash
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.181 'for i in $(seq 1 5); do curl -s -o /dev/null -w "%{http_code}" --max-time 15 -x socks5h://127.0.0.1:7000 http://httpbin.org/ip; echo " attempt $i"; done'
```

---

## Ограничения (ВСЕ секции)

- Менять ТОЛЬКО `crates/sp-server/src/main.rs` и `crates/sp-client/src/main.rs`
- НЕ менять retry loop структуру на клиенте
- НЕ менять multipath router
- НЕ добавлять новые crate зависимости
- НЕ писать комментарии с упоминанием AI/Claude
- Warnings допустимы, errors — нет
- `-o StrictHostKeyChecking=no` на ВСЕХ ssh/scp
- ВСЕГДА бэкапить сервер перед заменой (`cp sp-server sp-server.bak`)
