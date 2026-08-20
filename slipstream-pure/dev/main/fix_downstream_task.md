# Задача: Исправить downstream delivery (сервер + клиент)

## Корневая проблема

HTTPS success rate ~30%. Причина — комбинация двух проблем:

### Проблема 1: Hash dedup bug (клиент)

В `dispatch_downstream_inline` (crates/sp-client/src/main.rs, строка ~772):
```rust
let prev = last_ds_hash.swap(hash, Ordering::Relaxed);
if prev == hash { return false; }
```

Сравнивается только с ПРЕДЫДУЩИМ хешем. Pump шлёт 15 параллельных polls через разные resolvers.
Когда приходит batch 2, а потом ПОЗДНИЙ ответ batch 1 от медленного resolver'а:
- prev=H2 (от batch 2), current=H1 (от batch 1)
- H1 ≠ H2 → "new!" → `batch_id` инкрементируется → ЛОЖНЫЙ ACK
- Сервер думает batch 2 получен, переходит к batch 3
- Batch 2 ПОТЕРЯН → TLS handshake не завершится

### Проблема 2: Слишком маленький MAX_RAW (сервер)

В `take_downstream_data` (crates/sp-server/src/main.rs, строка 456):
```rust
const MAX_RAW: usize = 500;
```

TLS ServerHello ~3252 байт → ~14 downstream пакетов по 240 байт → 7 батчей по 500.
7 round-trip'ов через DNS. Каждый лишний — шанс для бага из Проблемы 1.

## Что нужно сделать

### Фикс 1: Клиент — ring buffer для hash dedup

Файл: `crates/sp-client/src/main.rs`

Заменить `last_ds_hash: AtomicU64` на ring buffer из 32 хешей.
Это lock-free, не требует Mutex.

**В pump** (в retry loop, где создаётся ds_hash, сейчас ~строка 487):
Было:
```rust
let ds_hash = Arc::new(std::sync::atomic::AtomicU64::new(0));
```
Стало:
```rust
let ds_hashes = Arc::new(DsHashRing::new());
```

**Новая структура** (добавить перед `handle_socks5_connection` или в начале файла):
```rust
struct DsHashRing {
    hashes: [std::sync::atomic::AtomicU64; 32],
    idx: std::sync::atomic::AtomicUsize,
}

impl DsHashRing {
    fn new() -> Self {
        Self {
            hashes: std::array::from_fn(|_| std::sync::atomic::AtomicU64::new(0)),
            idx: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn check_and_insert(&self, hash: u64) -> bool {
        // Check if hash exists in ring
        for h in &self.hashes {
            if h.load(std::sync::atomic::Ordering::Relaxed) == hash {
                return false; // duplicate
            }
        }
        // Insert at next position
        let pos = self.idx.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 32;
        self.hashes[pos].store(hash, std::sync::atomic::Ordering::Relaxed);
        true // new data
    }
}
```

**Изменить `dispatch_downstream_inline`**:
Сигнатура — заменить `last_ds_hash: &AtomicU64` на `ds_hashes: &DsHashRing`:
```rust
async fn dispatch_downstream_inline(
    txt_data: &[u8],
    streams: &Arc<RwLock<HashMap<u16, StreamDownstream>>>,
    ds_hashes: &DsHashRing,
) -> bool {
```

Тело — заменить блок hash check (строки ~772-781):
```rust
    // Было:
    // let prev = last_ds_hash.swap(hash, Ordering::Relaxed);
    // if prev == hash { return false; }

    // Стало:
    if !ds_hashes.check_and_insert(hash) {
        return false;
    }
```

**Обновить вызов dispatch_downstream_inline в pump** — передать `&*ds_hashes` вместо `&*ds_hash`.

**Обновить все места где создаётся/используется ds_hash** — поиск по файлу, заменить на ds_hashes.
Включая `_final_ds_hash` и подобные.

### Фикс 2: Сервер — увеличить MAX_RAW до 2000

Файл: `crates/sp-server/src/main.rs`

Строка 456:
```rust
// Было:
const MAX_RAW: usize = 500;
// Стало:
const MAX_RAW: usize = 2000;
```

Это уменьшит количество батчей для ServerHello с 7 до 2.
2000 байт raw → ~1400 compressed → ~1900 base64 → укладывается в EDNS0 (4096).

Это ОДНА строка. Других изменений в серверном коде не нужно.

## Доступы

### Клиент VPS
- IP: `203.0.113.181`
- User: `root`
- Password: `<REDACTED_PASSWORD>`
- SSH: `sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.181`

### Сервер VPS
- IP: `203.0.113.130`
- User: `root`
- Password: `<REDACTED_PASSWORD>`
- SSH: `sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.130`
- Сервер запущен из /opt: `./sp-server --domain tunnel.example.com --dns-listen 203.0.113.130:53`
- Лог: `/var/log/sp-server.log`

## Сборка

```bash
cd /work/vpn/slipstream-pure-dev && cargo build --release 2>&1
```

Без ошибок. Warnings допустимы.

## Деплой

### Сервер (НУЖНО обновить — MAX_RAW изменён):
```bash
sshpass -p '<REDACTED_PASSWORD>' scp -o StrictHostKeyChecking=no target/release/sp-server root@203.0.113.130:/opt/sp-server-new
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.130 'cd /opt && pkill sp-server; sleep 1; cp sp-server sp-server.bak; mv sp-server-new sp-server; chmod +x sp-server; nohup ./sp-server --domain tunnel.example.com --dns-listen 203.0.113.130:53 > /var/log/sp-server.log 2>&1 &'
sleep 3
```

### Клиент:
```bash
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.181 'pkill sp-client; sleep 1; rm -f /root/sp-client'
sshpass -p '<REDACTED_PASSWORD>' scp -o StrictHostKeyChecking=no target/release/sp-client root@203.0.113.181:/root/
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.181 'chmod +x /root/sp-client && RUST_LOG=info,sp_client=debug nohup /root/sp-client --domain tunnel.example.com --config /root/config/resolvers.toml > /var/log/sp-client.log 2>&1 &'
sleep 8
```

## Тестирование

```bash
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.181 'for i in $(seq 1 10); do curl -s -o /dev/null -w "%{http_code}" --max-time 35 -x socks5h://127.0.0.1:7000 https://httpbin.org/ip; echo " attempt $i"; done'
```

**Успех: 7+ из 10 HTTPS запросов возвращают 200.**

Проверка логов:
```bash
sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.181 'grep -E "(retry|attempt|got.*bytes|no downstream)" /var/log/sp-client.log | tail -20'
```

## Ограничения

- Менять ТОЛЬКО `crates/sp-client/src/main.rs` и `crates/sp-server/src/main.rs`
- На сервере менять ТОЛЬКО `MAX_RAW` (одна строка)
- НЕ писать комментарии с упоминанием AI/Claude
- НЕ добавлять новые файлы
- НЕ менять retry loop структуру — только hash dedup
- Warnings допустимы, errors — нет
- ОБЯЗАТЕЛЬНО задеплоить И сервер И клиент перед тестированием
