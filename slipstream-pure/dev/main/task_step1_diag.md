# Шаг 1: Диагностика downstream delivery

## Цель
Добавить debug-логи на СЕРВЕР чтобы понять: polls приходят? данные есть? batch отдаётся?

## Что делать

Файл: `crates/sp-server/src/main.rs`

Найди функцию `take_downstream_data` (~строка 454). Добавь debug! логи:

```rust
async fn take_downstream_data(session: &Session, client_ack: u32) -> Option<Vec<u8>> {
    const MAX_RAW: usize = 500;

    let mut batch_id = session.ds_batch_id.lock().await;
    let mut pending = session.pending_downstream.lock().await;

    // ДОБАВЬ ЭТУ СТРОКУ:
    let queue_len = session.downstream_queue.lock().await.len();
    debug!("downstream: ack={}, batch_id={}, pending={}, queue={}", client_ack, *batch_id, pending.is_some(), queue_len);

    // ... остальной код без изменений ...
```

Также найди `handle_tunnel_packet`, case `PacketType::Poll` (~строка 351). Сейчас там пустой блок `PacketType::Poll => {}`. Добавь лог:

```rust
PacketType::Poll => {
    debug!("poll from session {}", session.id);
}
```

## Сборка и деплой

```bash
cd /work/vpn/slipstream-pure-dev && cargo build --release 2>&1
```

Деплой ТОЛЬКО сервер:
```bash
sshpass -p '<REDACTED_PASSWORD>' scp -o StrictHostKeyChecking=no target/release/sp-server root@203.0.113.130:/opt/sp-server-new
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.130 'cd /opt && cp sp-server sp-server.bak && pkill sp-server; sleep 1; mv sp-server-new sp-server; chmod +x sp-server; RUST_LOG=info,sp_server=debug nohup ./sp-server --domain tunnel.example.com --dns-listen 203.0.113.130:53 > /var/log/sp-server.log 2>&1 &'
sleep 3
```

Перезапусти клиент (оригинальный бинарник):
```bash
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.181 'pkill -9 sp-client; sleep 2; rm -f /var/log/sp-client.log; RUST_LOG=info,sp_client=debug nohup /root/sp-client-orig --domain tunnel.example.com --config /root/config/resolvers.toml > /var/log/sp-client.log 2>&1 &'
sleep 8
```

## Тест

Прогони 5 HTTPS запросов:
```bash
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.181 'for i in $(seq 1 5); do curl -s -o /dev/null -w "%{http_code}" --max-time 35 -x socks5h://127.0.0.1:7000 https://httpbin.org/ip; echo " attempt $i"; done'
```

Затем посмотри серверные логи:
```bash
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.130 'grep "downstream:" /var/log/sp-server.log | tail -30'
```

## Результат

Запиши в файл `/work/vpn/slipstream-pure-dev/diag_results.md`:
- Сколько polls пришло
- Были ли данные в queue когда poll приходил
- Какие batch_id и ack видны
- Твой вывод: где именно теряются данные

## Ограничения
- Менять ТОЛЬКО sp-server/src/main.rs
- ТОЛЬКО добавить debug! логи (2 строки)
- НЕ менять логику, НЕ менять клиент
- НЕ писать комментарии с AI/Claude
