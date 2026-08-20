# Задача: Исправить отправку initial data в retry loop

## Контекст

Файл: `/work/vpn/slipstream-pure-dev/crates/sp-client/src/main.rs`

В `handle_socks5_connection` (строка ~363) реализован connection-level retry для HTTPS.
Retry работает, но success rate низкий (~30%). Причина найдена:

**Баг**: initial data (TLS ClientHello, ~517 байт, ~8 chunks) отправляется ПОСЛЕДОВАТЕЛЬНО —
каждый chunk ждёт завершения `send_parallel_good` с timeout 800ms. Итого до 6.4 секунд
только на отправку. Это съедает весь timeout для ожидания downstream (6 сек), и retry
не успевает.

В оригинальном коде (upstream task, строка ~610) chunks отправляются ПАРАЛЛЕЛЬНО через
`tokio::spawn` — все сразу, fire-and-forget. Это занимает ~10ms.

## Что нужно сделать

### 1. Заменить последовательную отправку initial data на параллельную

Текущий код (строки ~548-564):
```rust
        // Send buffered initial client data (ClientHello) through multiple paths for redundancy
        if !initial_data.is_empty() {
            let seq_base: u32 = 1;
            for (i, chunk) in initial_data.chunks(chunk_size).enumerate() {
                let s = seq_base.wrapping_add(i as u32);
                let packet = TunnelPacket::new_data(stream_id, s, Bytes::copy_from_slice(chunk));
                let encoded = packet.encode();
                let compressed = compress::compress(&encoded);
                if let Ok(query) = build_query(&compressed, &client.session_id, &client.domain) {
                    // Send through 5 good paths in parallel for high delivery probability
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_millis(800),
                        client.router.send_parallel_good(&query, 5),
                    ).await;
                }
            }
            debug!("sent {} bytes initial data via parallel paths", initial_data.len());
        }
```

Заменить на параллельную отправку через `tokio::spawn` (как в upstream task):
```rust
        if !initial_data.is_empty() {
            let seq_base: u32 = 1;
            let mut send_handles = Vec::new();
            for (i, chunk) in initial_data.chunks(chunk_size).enumerate() {
                let s = seq_base.wrapping_add(i as u32);
                let packet = TunnelPacket::new_data(stream_id, s, Bytes::copy_from_slice(chunk));
                let encoded = packet.encode();
                let compressed = compress::compress(&encoded);
                if let Ok(query) = build_query(&compressed, &client.session_id, &client.domain) {
                    let router_c = client.router.clone();
                    send_handles.push(tokio::spawn(async move {
                        // Send through good path with retries (like upstream task does)
                        for _attempt in 0..5u8 {
                            match router_c.send_via_good_paths(&query).await {
                                Ok(_) => break,
                                Err(e) => debug!("initial data send error: {}", e),
                            }
                        }
                    }));
                }
            }
            // Wait for all chunks to be sent (with timeout)
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                futures::future::join_all(send_handles),
            ).await;
            debug!("sent {} bytes initial data in parallel", initial_data.len());
        }
```

**Важно**: проверь что `futures` есть в зависимостях sp-client. Если нет — используй простой подход: не ждать handles вообще (fire-and-forget как в upstream), или ждать каждый через `tokio::join!`, или просто собрать в вектор и `for h in handles { let _ = h.await; }` с общим timeout.

Альтернатива без futures (проще):
```rust
        if !initial_data.is_empty() {
            let seq_base: u32 = 1;
            for (i, chunk) in initial_data.chunks(chunk_size).enumerate() {
                let s = seq_base.wrapping_add(i as u32);
                let chunk_data = Bytes::copy_from_slice(chunk);
                let router_c = client.router.clone();
                let sid = client.session_id.clone();
                let dom = client.domain.clone();
                tokio::spawn(async move {
                    let packet = TunnelPacket::new_data(stream_id, s, chunk_data);
                    let encoded = packet.encode();
                    let compressed = compress::compress(&encoded);
                    let query = match build_query(&compressed, &sid, &dom) {
                        Ok(q) => q,
                        Err(_) => return,
                    };
                    for _attempt in 0..5u8 {
                        match router_c.send_via_good_paths(&query).await {
                            Ok(_) => break,
                            Err(e) => debug!("initial data send error: {}", e),
                        }
                    }
                });
            }
            // Small delay to let spawned tasks start sending
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            debug!("spawned {} initial data chunks", initial_data.chunks(chunk_size).count());
        }
```

### 2. Не менять ничего другого

Retry loop, pump, upstream task, downstream writer — всё оставить как есть.

## Доступы

### Клиент VPS (где запускать)
- IP: `203.0.113.181`
- User: `root`
- Password: `<REDACTED_PASSWORD>`
- SSH: `sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.181`
- Бинарник: `/root/sp-client`
- Конфиг: `/root/config/resolvers.toml`
- Лог: `/var/log/sp-client.log`

### Сервер VPS (не менять, только для проверки логов)
- IP: `203.0.113.130`
- User: `root`
- Password: `<REDACTED_PASSWORD>`
- Бинарник: `/opt/sp-server`
- Лог: `/var/log/sp-server.log`

## Сборка и деплой

```bash
cd /work/vpn/slipstream-pure-dev && cargo build --release 2>&1
```
Должно скомпилироваться без ошибок.

```bash
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.181 'pkill sp-client; sleep 1; rm -f /root/sp-client'
sshpass -p '<REDACTED_PASSWORD>' scp -o StrictHostKeyChecking=no /work/vpn/slipstream-pure-dev/target/release/sp-client root@203.0.113.181:/root/
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.181 'chmod +x /root/sp-client && RUST_LOG=info,sp_client=debug nohup /root/sp-client --domain tunnel.example.com --config /root/config/resolvers.toml > /var/log/sp-client.log 2>&1 &'
sleep 8
```

## Тестирование

```bash
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.181 'for i in $(seq 1 10); do curl -s -o /dev/null -w "%{http_code}" --max-time 35 -x socks5h://127.0.0.1:7000 https://httpbin.org/ip; echo " attempt $i"; done'
```

Успех: **7+ из 10** HTTPS запросов возвращают 200.

Проверка логов retry:
```bash
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.181 'grep -E "(retry|attempt|got.*bytes|no downstream)" /var/log/sp-client.log | tail -30'
```

## Ограничения

- Менять ТОЛЬКО `crates/sp-client/src/main.rs`
- Менять ТОЛЬКО блок отправки initial data (строки ~548-564)
- НЕ менять retry loop, pump, upstream task, downstream writer
- НЕ добавлять новые файлы
- НЕ писать комментарии с упоминанием AI/Claude
- Warnings допустимы, errors — нет
