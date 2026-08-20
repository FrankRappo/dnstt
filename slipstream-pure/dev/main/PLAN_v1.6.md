# Plan v1.6: Sliding Window + Selective ACK + FEC

## Цель
HTTPS: 30% → 99%+, скорость: 10 → 20+ KB/s на один поток.
Превзойти QUIC через DNS за счёт заточки под DNS-канал.

## Текущие проблемы (диагностика 2026-03-25)

### Проблема: stop-and-wait downstream
Сервер `take_downstream_data` (sp-server/main.rs:454):
- Отправляет 1 batch (≤500 байт)
- Ждёт ACK от клиента
- Только потом отправляет следующий

TLS ServerHello = ~3252 байт = ~14 пакетов по 240 байт = **7 batch'ей**.
Каждый batch = 1 DNS round-trip (~200-500ms).
7 round-trips = 1.5-3.5 секунды **при 0% потерь**.

При 10% потерь DNS-ответов: каждый потерянный ответ = повторный round-trip.
P(все 7 batch'ей доставлены без потерь) = 0.9^7 = **48%**.
При 20% потерь: 0.8^7 = **21%** — совпадает с наблюдаемыми ~30%.

### Почему retry не помогает
Retry пересоздаёт соединение заново (новый Connect + ClientHello).
Но DNS условия не меняются за 3-5 секунд — те же resolver'ы, те же потери.
Каждая retry-попытка имеет те же ~30% шансы.

### Сравнение с QUIC
QUIC через DNS имел бы те же DNS-потери ПЛЮС:
- QUIC header overhead (20-40 байт/пакет)
- QUIC congestion control не понимает DNS relay (RTT 100-500ms, не 10-50ms)
- QUIC retransmission рассчитан на direct UDP, не на relay через resolver

Наш протокол может быть ЛУЧШЕ QUIC потому что заточен под DNS-канал.

---

## Фаза 1: Sliding Window (главный фикс)

### Идея
Вместо "отправил batch → жди ACK → следующий" (stop-and-wait):
- Сервер отправляет до W batch'ей вперёд (window size W=4)
- Клиент шлёт ACK с номером последнего полученного batch'а
- Сервер переотправляет только batch'и которые за пределами ACK

### Изменения на сервере (sp-server/main.rs)

**Заменить `pending_downstream: Mutex<Option<Vec<u8>>>` на sliding window:**

```rust
struct Session {
    id: String,
    streams: tokio::sync::Mutex<HashMap<u16, StreamHandle>>,
    downstream_queue: tokio::sync::Mutex<Vec<Bytes>>,
    // БЫЛО: pending_downstream: Mutex<Option<Vec<u8>>>,
    // СТАЛО:
    window: tokio::sync::Mutex<DownstreamWindow>,
}

struct DownstreamWindow {
    batches: Vec<(u32, Vec<u8>)>,  // (batch_id, data)
    next_batch_id: u32,
    window_size: usize,            // max batches ahead of client ACK
}
```

**Новая `take_downstream_data`:**
```rust
async fn take_downstream_data(session: &Session, client_ack: u32) -> Option<Vec<u8>> {
    const MAX_RAW: usize = 500;
    const WINDOW_SIZE: usize = 4;

    let mut window = session.window.lock().await;

    // Удалить ACK'нутые batch'и
    window.batches.retain(|(id, _)| *id > client_ack);

    // Если есть не-ACK'нутые — переотправить первый (старейший)
    if let Some((id, data)) = window.batches.first() {
        return Some(data.clone());
    }

    // Окно не заполнено — взять новые данные из очереди
    if window.batches.len() >= WINDOW_SIZE {
        return None; // окно полное, ждём ACK
    }

    let mut queue = session.downstream_queue.lock().await;
    if queue.is_empty() {
        return None;
    }

    // Собрать batch из очереди
    let mut total = 0;
    let mut taken = Vec::new();
    while let Some(data) = queue.first() {
        if total > 0 && total + data.len() > MAX_RAW { break; }
        total += data.len();
        taken.push(queue.remove(0));
    }

    if taken.is_empty() { return None; }

    window.next_batch_id += 1;
    let batch_id = window.next_batch_id;

    let mut combined = Vec::with_capacity(total);
    for chunk in taken {
        combined.extend_from_slice(&chunk);
    }

    window.batches.push((batch_id, combined.clone()));
    Some(combined)
}
```

**Суть:** сервер шлёт batch'и непрерывно, до 4 штук вперёд.
При потере одного — он будет переотправлен при следующем poll.
Клиент ACK'ает по мере получения.

**Преимущество:** 7 batch'ей для ServerHello доставляются за 2-3 poll-цикла вместо 7.

### Изменения на клиенте (sp-client/main.rs)

**ACK логика в pump — отправлять номер последнего НЕПРЕРЫВНО полученного batch'а:**

Сейчас клиент инкрементирует `batch_id` при каждом новом (non-duplicate) ответе.
Это уже работает корректно со sliding window — ACK = "я получил всё до этого номера".

Клиенту менять почти ничего:
- Hash ring dedup (уже добавлен) защищает от ложных ACK
- batch_id инкрементируется только при новых данных — это правильно

**Но** есть проблема: клиент не знает batch_id сервера. Он считает свой локальный счётчик.
Если сервер отправил batch 1,2,3,4 а клиент получил 1,3 (пропустил 2):
- Клиент видит: new→ACK=1, new→ACK=2 (но это batch 3!)
- Сервер получает ACK=2, думает 1,2 доставлены, удаляет их
- Batch 2 потерян!

**Решение:** сервер добавляет batch_id как 4-байтный префикс в данные.

### Протокольное изменение: batch_id в downstream

**Сервер:** при формировании batch, добавить 4 байта batch_id в начало:
```rust
let mut combined = Vec::with_capacity(4 + total);
combined.extend_from_slice(&batch_id.to_be_bytes());
// ... затем данные пакетов
```

**Клиент:** в `dispatch_downstream_inline`, парсить первые 4 байта как batch_id:
```rust
if decompressed.len() < 4 { return false; }
let server_batch_id = u32::from_be_bytes([
    decompressed[0], decompressed[1], decompressed[2], decompressed[3],
]);
let packet_data = &decompressed[4..];

// ACK = максимальный непрерывно полученный batch_id
// Нужен ordered set полученных batch_id:
// received: {1, 3, 4} → ACK = 1 (пропущен 2)
// received: {1, 2, 3, 4} → ACK = 4
```

**Клиентская структура для tracking:**
```rust
struct BatchTracker {
    received: std::collections::BTreeSet<u32>,
    ack: u32,  // max contiguous received
}

impl BatchTracker {
    fn receive(&mut self, batch_id: u32) -> bool {
        if batch_id <= self.ack { return false; } // already ACK'd
        let is_new = self.received.insert(batch_id);
        // Advance ACK
        while self.received.contains(&(self.ack + 1)) {
            self.ack += 1;
            self.received.remove(&self.ack);
        }
        is_new
    }
}
```

---

## Фаза 2: FEC (Forward Error Correction)

### Идея
На каждые K batch'ей сервер отправляет 1 parity batch (XOR всех K).
Если клиент получил K-1 из K + parity, он восстанавливает пропущенный.

### Параметры
K=4: на каждые 4 batch'а — 1 parity. Overhead 25%.
Позволяет восстановить 1 потерю из 5 без retransmission.

### Реализация (сервер)
```rust
// После каждых 4 batch'ей — отправить parity
if window.next_batch_id % 5 == 0 {
    let parity = xor_batches(&window.batches[last_4..]);
    window.batches.push((window.next_batch_id, parity));
}
```

### Реализация (клиент)
```rust
// Если получили 4 из 5 (включая parity) — восстановить пропущенный
if can_recover(received_batches, parity_batch) {
    let missing = xor_recover(received_batches, parity_batch);
    // dispatch missing
}
```

**Эффект:** при 10% потерь P(доставка 7 batch'ей) растёт с 48% до ~95%.
С retry: 99.8%+.

---

## Фаза 3: N sub-streams (v2.0)

После sliding window + FEC:
- Каждый sub-stream использует свой набор resolver'ов
- N × 20 KB/s = 100+ KB/s при N=5
- Независимые ACK-потоки

---

## Порядок реализации

### Шаг 1: Диагностика (быстро, 30 минут)
Добавить логирование на сервере:
```bash
# Проверить что сервер получает polls:
grep "poll" /var/log/sp-server.log
# Сколько batch'ей в очереди при poll:
# Какой client_ack приходит
```
Добавить в `take_downstream_data`:
```rust
debug!("take_downstream: client_ack={}, batch_id={}, queue_len={}, pending={}",
    client_ack, *batch_id, queue.len(), pending.is_some());
```
Задеплоить, прогнать тест, посмотреть логи.

### Шаг 2: Batch ID prefix (30 минут)
- Сервер: добавить 4-байтный batch_id в начало downstream данных
- Клиент: парсить batch_id, заменить hash dedup на batch tracking
- Это фикс ложных ACK и подготовка к sliding window

### Шаг 3: Sliding window (1-2 часа)
- Сервер: заменить pending на window (Vec<(u32, Vec<u8>)>)
- Клиент: BatchTracker с ordered set
- Window size = 4

### Шаг 4: Тест и tuning (30 минут)
- Прогнать 10× HTTPS тест
- Подобрать window size (2, 4, 6, 8)
- Подобрать MAX_RAW (400, 500, 600)

### Шаг 5: FEC (опционально, 1-2 часа)
- Только если sliding window не даёт 95%+
- XOR parity каждые 4 batch'а

---

## Доступы

### Клиент VPS
- IP: `203.0.113.181`, root, `203.0.113.130`

### Сервер VPS
- IP: `203.0.113.130`, root, `203.0.113.130`
- Бинарник: `/opt/sp-server`
- Бэкап: `/opt/sp-server.bak`

## Сборка и деплой

```bash
# Сборка
cd /work/vpn/slipstream-pure-dev && cargo build --release 2>&1

# Деплой сервера
sshpass -p '<REDACTED_PASSWORD>' scp target/release/sp-server root@203.0.113.130:/opt/sp-server-new
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.130 'cd /opt && cp sp-server sp-server.bak && pkill sp-server; sleep 1; mv sp-server-new sp-server; chmod +x sp-server; nohup ./sp-server --domain tunnel.example.com --dns-listen 203.0.113.130:53 > /var/log/sp-server.log 2>&1 &'

# Деплой клиента
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.181 'pkill -9 sp-client; sleep 2; rm -f /root/sp-client'
sshpass -p '<REDACTED_PASSWORD>' scp target/release/sp-client root@203.0.113.181:/root/
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.181 'chmod +x /root/sp-client && rm -f /var/log/sp-client.log && RUST_LOG=info,sp_client=debug,sp_server=debug nohup /root/sp-client --domain tunnel.example.com --config /root/config/resolvers.toml > /var/log/sp-client.log 2>&1 &'
sleep 8
```

## Тест

```bash
# HTTPS (главный тест)
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.181 'for i in $(seq 1 10); do curl -s -o /dev/null -w "%{http_code}" --max-time 35 -x socks5h://127.0.0.1:7000 https://httpbin.org/ip; echo " attempt $i"; done'

# HTTP (контрольный, должен быть ~100%)
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.181 'for i in $(seq 1 5); do curl -s -o /dev/null -w "%{http_code}" --max-time 15 -x socks5h://127.0.0.1:7000 http://httpbin.org/ip; echo " attempt $i"; done'
```

## Ограничения для агента

- Добавить `-o StrictHostKeyChecking=no` ко ВСЕМ ssh/scp командам
- НЕ писать комментарии с упоминанием AI/Claude
- НЕ менять retry loop в клиенте (оставить как есть)
- НЕ менять multipath router
- НЕ добавлять новые crate зависимости
- Warnings допустимы, errors — нет
- ОБЯЗАТЕЛЬНО задеплоить ОБОИХ (сервер И клиент) перед тестированием
- ОБЯЗАТЕЛЬНО сначала бэкапить сервер (`cp sp-server sp-server.bak`)
