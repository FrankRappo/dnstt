# Шаг 2: Batch ID prefix в downstream

## Предварительно
Прочитай результаты диагностики: `cat /work/vpn/slipstream-pure-dev/diag_results.md`
Если файла нет — сначала выполни task_step1_diag.md.

## Цель
Сервер маркирует каждый batch 4-байтным ID. Клиент парсит ID и трекает получение.
Это устраняет баг ложных ACK и готовит к sliding window.

## Что делать на СЕРВЕРЕ

Файл: `crates/sp-server/src/main.rs`

В функции `take_downstream_data`, после формирования `combined`, добавь batch_id как префикс:

Найди этот блок (~строка 488-496):
```rust
    *batch_id = batch_id.wrapping_add(1);

    let mut combined = Vec::with_capacity(total);
    for chunk in taken {
        combined.extend_from_slice(&chunk);
    }

    *pending = Some(combined.clone());
    Some(combined)
```

Замени на:
```rust
    *batch_id = batch_id.wrapping_add(1);

    let mut combined = Vec::with_capacity(4 + total);
    combined.extend_from_slice(&batch_id.to_be_bytes());
    for chunk in taken {
        combined.extend_from_slice(&chunk);
    }

    *pending = Some(combined.clone());
    Some(combined)
```

Изменение: одна строка добавлена (`combined.extend_from_slice(&batch_id.to_be_bytes())`), capacity +4.

## Что делать на КЛИЕНТЕ

Файл: `crates/sp-client/src/main.rs`

### 2a. Добавить BatchTracker (перед функцией handle_socks5_connection)

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

### 2b. Заменить DsHashRing на BatchTracker в pump

В retry loop, где создаётся `ds_hashes`, замени на BatchTracker.

Найди (примерно строка 512):
```rust
        let ds_hashes = Arc::new(DsHashRing::new());
```
Замени:
```rust
        let batch_tracker = Arc::new(tokio::sync::Mutex::new(BatchTracker::new()));
```

В pump spawn, замени clone:
```rust
        // БЫЛО: let ds_hashes = ds_hashes.clone();
        let batch_tracker = batch_tracker.clone();
```

### 2c. Заменить dispatch в pump

Найди блок в pump (~строка 551-560):
```rust
                            let is_new = dispatch_downstream_inline(
                                resp.txt_data.as_ref().unwrap(),
                                &streams,
                                &*ds_hashes,
                            ).await;
                            router.report_downstream_success(pid).await;
                            if is_new {
                                batch_id.fetch_add(1, Ordering::Relaxed);
                            }
```

Замени на:
```rust
                            let recv_batch_id = dispatch_downstream_with_batch(
                                resp.txt_data.as_ref().unwrap(),
                                &streams,
                                &batch_tracker,
                            ).await;
                            if recv_batch_id > 0 {
                                router.report_downstream_success(pid).await;
                                let tracker = batch_tracker.lock().await;
                                batch_id.store(tracker.ack, Ordering::Relaxed);
                            }
```

### 2d. Новая функция dispatch_downstream_with_batch

Добавь ПЕРЕД или ПОСЛЕ существующей `dispatch_downstream_inline`:

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

### 2e. Обновить _final_ds_hashes

Найди `_final_ds_hashes` и замени на `_final_batch_tracker`:
```rust
// БЫЛО: let mut _final_ds_hashes = Arc::new(DsHashRing::new());
let mut _final_batch_tracker: Option<Arc<tokio::sync::Mutex<BatchTracker>>> = None;

// В success branch:
// БЫЛО: _final_ds_hashes = ds_hashes;
_final_batch_tracker = Some(batch_tracker);
```

### 2f. Можно удалить DsHashRing и dispatch_downstream_inline
Они больше не используются. Удали struct DsHashRing и fn dispatch_downstream_inline.
Если что-то ещё ссылается на них — исправь.

## Сборка
```bash
cd /work/vpn/slipstream-pure-dev && cargo build --release 2>&1
```
Без ошибок. Warnings допустимы.

## Деплой (ОБОИХ!)

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

## Тест
```bash
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.181 'for i in $(seq 1 10); do curl -s -o /dev/null -w "%{http_code}" --max-time 35 -x socks5h://127.0.0.1:7000 https://httpbin.org/ip; echo " attempt $i"; done'
```

Результат должен быть примерно как baseline (~30%) — этот шаг не улучшает скорость, а фиксит ACK tracking. Запиши результат в `/work/vpn/slipstream-pure-dev/step2_results.md`.

## Ограничения
- Менять ТОЛЬКО sp-server/src/main.rs и sp-client/src/main.rs
- НЕ менять retry loop структуру
- НЕ менять multipath router
- НЕ добавлять новые crate зависимости
- НЕ писать комментарии с AI/Claude
- Warnings допустимы, errors — нет
