# Шаг 3: Sliding Window

## Предварительно
Прочитай результат шага 2: `cat /work/vpn/slipstream-pure-dev/step2_results.md`
Если файла нет — сначала выполни task_step2_batchid.md.

Прочитай текущий код сервера: `crates/sp-server/src/main.rs`
Убедись что batch_id prefix уже добавлен (шаг 2 выполнен).

## Цель
Заменить stop-and-wait на sliding window. Сервер шлёт до W=4 batch'ей вперёд не дожидаясь ACK.
Это главный фикс: HTTPS 30% → 80%+.

## Что делать на СЕРВЕРЕ

Файл: `crates/sp-server/src/main.rs`

### 3a. Новая структура вместо pending

Найди struct Session (~строка 36):
```rust
struct Session {
    id: String,
    streams: tokio::sync::Mutex<HashMap<u16, StreamHandle>>,
    downstream_queue: tokio::sync::Mutex<Vec<Bytes>>,
    pending_downstream: tokio::sync::Mutex<Option<Vec<u8>>>,
    ds_batch_id: tokio::sync::Mutex<u32>,
}
```

Замени на:
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

struct Session {
    id: String,
    streams: tokio::sync::Mutex<HashMap<u16, StreamHandle>>,
    downstream_queue: tokio::sync::Mutex<Vec<Bytes>>,
    window: tokio::sync::Mutex<DownstreamWindow>,
}
```

### 3b. Обновить создание Session

Найди `get_or_create_session` (~строка 269), где создаётся Session:
```rust
    let session = Arc::new(Session {
        id: session_id.to_string(),
        streams: tokio::sync::Mutex::new(HashMap::new()),
        downstream_queue: tokio::sync::Mutex::new(Vec::new()),
        pending_downstream: tokio::sync::Mutex::new(None),
        ds_batch_id: tokio::sync::Mutex::new(0),
    });
```

Замени на:
```rust
    let session = Arc::new(Session {
        id: session_id.to_string(),
        streams: tokio::sync::Mutex::new(HashMap::new()),
        downstream_queue: tokio::sync::Mutex::new(Vec::new()),
        window: tokio::sync::Mutex::new(DownstreamWindow::new()),
    });
```

### 3c. Обновить handle_tunnel_packet (Connect)

Найди в Connect handler (~строка 304-306):
```rust
                *session.pending_downstream.lock().await = None;
                *session.ds_batch_id.lock().await = 0;
                session.downstream_queue.lock().await.clear();
```

Замени на:
```rust
                *session.window.lock().await = DownstreamWindow::new();
                session.downstream_queue.lock().await.clear();
```

### 3d. Новая take_downstream_data

Замени ВСЮ функцию `take_downstream_data` на:

```rust
async fn take_downstream_data(session: &Session, client_ack: u32) -> Option<Vec<u8>> {
    const MAX_RAW: usize = 500;
    const WINDOW_SIZE: usize = 4;

    let mut win = session.window.lock().await;

    // Удалить batch'и которые клиент уже получил
    if client_ack > 0 {
        win.batches.retain(|(id, _)| *id > client_ack);
    }

    // Переотправить самый старый неподтверждённый batch
    if let Some((_id, data)) = win.batches.first() {
        return Some(data.clone());
    }

    // Окно не заполнено — взять новые данные
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

### 3e. Исправить ошибки компиляции

После замен, `pending_downstream` и `ds_batch_id` больше не существуют.
Поищи их usage: `grep -n "pending_downstream\|ds_batch_id" crates/sp-server/src/main.rs`
Если найдутся — удали или замени на `window`.

## Клиент — изменений НЕ нужно
Клиент уже использует BatchTracker из шага 2. Sliding window совместим.

## Сборка
```bash
cd /work/vpn/slipstream-pure-dev && cargo build --release 2>&1
```

## Деплой (ТОЛЬКО сервер!)

```bash
sshpass -p '<REDACTED_PASSWORD>' scp -o StrictHostKeyChecking=no target/release/sp-server root@203.0.113.130:/opt/sp-server-new
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.130 'cd /opt && cp sp-server sp-server.bak && pkill sp-server; sleep 1; mv sp-server-new sp-server; chmod +x sp-server; RUST_LOG=info,sp_server=debug nohup ./sp-server --domain tunnel.example.com --dns-listen 203.0.113.130:53 > /var/log/sp-server.log 2>&1 &'
sleep 3
```

Перезапустить клиент (тот же бинарник из шага 2, НЕ пересобирать):
```bash
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.181 'pkill -9 sp-client; sleep 2; rm -f /var/log/sp-client.log; RUST_LOG=info,sp_client=debug nohup /root/sp-client --domain tunnel.example.com --config /root/config/resolvers.toml > /var/log/sp-client.log 2>&1 &'
sleep 8
```

## Тест
```bash
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.181 'for i in $(seq 1 10); do curl -s -o /dev/null -w "%{http_code}" --max-time 35 -x socks5h://127.0.0.1:7000 https://httpbin.org/ip; echo " attempt $i"; done'
```

**Ожидаемый результат: 7+ из 10 HTTPS = 200.**

Если меньше 7 — проверь серверные логи:
```bash
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.130 'grep "downstream:" /var/log/sp-server.log | tail -20'
```

Запиши результат в `/work/vpn/slipstream-pure-dev/step3_results.md`.

## Ограничения
- Менять ТОЛЬКО sp-server/src/main.rs
- НЕ менять клиент (он уже готов из шага 2)
- НЕ менять multipath router
- НЕ добавлять зависимости
- НЕ писать комментарии с AI/Claude
