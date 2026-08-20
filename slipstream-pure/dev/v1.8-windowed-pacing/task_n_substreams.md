# Task: N Sub-Streams (v2.0) — Striping Downstream через N групп resolver'ов

## Цель

Один TCP-поток (SOCKS5 соединение) должен идти через N групп resolver'ов параллельно.
Сервер распределяет downstream chunks round-robin по sub-sessions.
Клиент собирает из всех sub-sessions по seq_num (reorder_buf уже есть).

Ожидаемый результат: скорость × N (N sub-sessions × 25 KB/s each).

## Текущая архитектура (что есть)

### Проект
- Рабочая директория: `/work/vpn/dnstt/slipstream-pure/dev/v1.8-windowed-pacing/`
- Язык: Rust, workspace из crate'ов
- Crate'ы: `sp-common`, `sp-client`, `sp-server`, `sp-multipath`, `sp-transport`, `sp-scanner`

### Как работает сейчас (1 сессия)
1. Клиент создаёт ONE session с session_id
2. Клиент шлёт burst из 3 poll'ов (PIPELINE=3), await all
3. Сервер: DownstreamWindow с WINDOW_SIZE=4, round-robin rotation
4. Сервер: stream_forward_task читает TCP в chunks, пишет в ONE downstream_queue
5. Клиент: BatchTracker (BTreeSet) собирает batch'и, вычисляет contiguous ACK
6. Клиент: TunnelPacket.seq_num — per-stream sequential numbering

### Ключевые параметры (проверены, НЕ менять)
- PIPELINE_WIDTH = 3 (= WINDOW_SIZE - 1)
- WINDOW_SIZE = 4
- timeout = 500ms
- NO sleep между burst'ами (RTT ~50ms сам пейсит)

### Ключевые структуры

**sp-common/src/protocol.rs:**
```rust
pub struct TunnelPacket {
    pub packet_type: PacketType,
    pub stream_id: u16,
    pub seq_num: u32,     // ← глобальный per-stream, используется для reassembly
    pub payload: Bytes,
}

pub enum PacketType {
    Data = 0x01, Poll = 0x02, Hello = 0x03, Bye = 0x04,
    Connect = 0x05, Disconnect = 0x06,
}
```

Hello payload format: `"session_id max_raw=3500 chunk=1500"`

**sp-server/src/main.rs:**
```rust
struct Session {
    id: String,
    streams: Mutex<HashMap<u16, StreamHandle>>,
    downstream_queue: Mutex<Vec<Bytes>>,
    window: Mutex<DownstreamWindow>,
    max_raw: AtomicUsize,    // per-session, из Hello
    chunk_size: AtomicUsize, // per-session, из Hello
}

struct DownstreamWindow {
    batches: Vec<(u32, Vec<u8>)>,  // (batch_id, data_with_prefix)
    next_batch_id: u32,
    send_cursor: usize,
}
```

Сервер `stream_forward_task`: читает TCP → 480B chunks → TunnelPacket → session.downstream_queue
Сервер `take_downstream_data`: заполняет window из queue, rotation по send_cursor.

**sp-client/src/main.rs:**
```rust
struct TunnelClient {
    domain: String,
    session_id: String,
    router: Arc<MultipathRouter>,
    seq: AtomicU32,
    stream_id_counter: AtomicU16,
    streams: Arc<RwLock<HashMap<u16, StreamDownstream>>>,
    chunk_size: usize,
    ds_max_raw: usize,
    ds_chunk_size: usize,
}
```

Клиент pump loop: burst 3 polls → await all → dispatch_downstream_with_batch → BatchTracker → ACK.

**sp-common/src/config.rs:**
```rust
pub struct ResolverConfig {
    pub name: String,
    pub ip: Vec<String>,
    pub doh: Option<String>,
    pub ech: bool,
    pub edns0_max: Option<u16>,
    pub rate_limit_rps: Option<u32>,
}

pub struct ResolverEndpoint {
    pub name: String,
    pub ip: String,
    pub doh_url: Option<String>,
    pub ech_supported: bool,
    pub edns0_max: Option<u16>,
    pub rate_limit_rps: Option<u32>,
}
```

**sp-multipath/src/lib.rs:**
- `MultipathRouter` — round-robin через paths, good_paths tracking
- `VirtualPath` — endpoint + transport + metrics + pacer
- `AdaptivePacer` — backoff on failure, recovery on success

## Архитектура N Sub-Streams

### Принцип

```
СЕРВЕР:
  TCP stream ← httpbin (50KB file)
       ↓
  chunk seq=1 → sub-session 0 queue → window W=4
  chunk seq=2 → sub-session 1 queue → window W=4
  chunk seq=3 → sub-session 2 queue → window W=4
  chunk seq=4 → sub-session 0 queue → window W=4
       ...      (round-robin)

КЛИЕНТ:
  sub-session 0 (resolvers 1-3) pump → data с seq=1,4,7...
  sub-session 1 (resolvers 4-6) pump → data с seq=2,5,8...
  sub-session 2 (resolvers 7-9) pump → data с seq=3,6,9...
       ↓            ↓            ↓
       └──────── reorder_buf (BTreeMap) ────────┘
                     ↓
              SOCKS5 stream (упорядоченные данные)
```

### Протокол — расширение Hello

```
"session_id group=GROUP_ID sub=0 total=3 max_raw=3500 chunk=1500"
```

- `group` — ID группы, общий для всех sub-sessions одного клиента
- `sub` — индекс этой sub-session (0..total-1)
- `total` — сколько sub-sessions в группе

Обратная совместимость: без `group` — работает как сейчас (одна сессия).

### Серверные изменения

**1. Новая структура SessionGroup (sp-server/src/main.rs):**

```rust
struct SessionGroup {
    group_id: String,
    sub_sessions: Vec<Arc<Session>>,  // упорядочены по sub index
    total: usize,
    // Общие streams для группы — TCP-соединения расшарены
    streams: Mutex<HashMap<u16, StreamHandle>>,
    stream_id_counter: AtomicU16,
}
```

**2. Маппинг sessions → groups:**

```rust
type SessionGroups = Arc<RwLock<HashMap<String, Arc<SessionGroup>>>>;
```

При получении Hello с `group=XXX sub=0 total=3`:
- Если группа XXX не существует — создать SessionGroup с total=3
- Зарегистрировать эту session как sub_sessions[0]
- Когда все sub_sessions зарегистрированы — группа готова

**3. stream_forward_task — round-robin по sub-sessions:**

Сейчас:
```rust
session.downstream_queue.lock().await.push(packet.encode());
```

Надо:
```rust
// Определить группу этой сессии
if let Some(group) = get_session_group(session_id) {
    // Round-robin по sub-sessions группы
    let sub_idx = ds_seq as usize % group.total;
    let sub_session = &group.sub_sessions[sub_idx];
    sub_session.downstream_queue.lock().await.push(packet.encode());
} else {
    // Обычная сессия без группы (обратная совместимость)
    session.downstream_queue.lock().await.push(packet.encode());
}
```

**4. Connect обработка:**

Connect должен идти через SessionGroup (общие streams), а не через отдельную session.
Когда poll с Connect приходит на любую sub-session — создаётся stream в группе.
TCP-соединение одно, downstream раздаётся round-robin.

**5. Upstream routing:**

Poll с Data может прийти на любую sub-session. Сервер должен найти StreamHandle
в группе (не в отдельной session). Upstream данные идут в общий TCP stream.

### Клиентские изменения

**1. Разбиение resolver'ов на группы:**

```rust
// N resolver endpoints → K sub-sessions по 1-3 resolver'а
fn split_into_subsessions(endpoints: &[ResolverEndpoint], resolvers_per_group: usize) -> Vec<Vec<ResolverEndpoint>> {
    endpoints.chunks(resolvers_per_group).map(|c| c.to_vec()).collect()
}
```

**2. Создание sub-sessions:**

```rust
let group_id = generate_group_id();  // random hex
let groups = split_into_subsessions(&endpoints, 3);  // по 3 resolver'а
let total = groups.len();

for (i, group_endpoints) in groups.iter().enumerate() {
    let session_id = generate_session_id();
    // Создать MultipathRouter для этой группы
    let router = Arc::new(MultipathRouter::new());
    for ep in group_endpoints {
        // добавить paths (UDP, TCP, DoH)
    }
    // Отправить Hello с group info
    let hello = format!("{} group={} sub={} total={} max_raw={} chunk={}",
        session_id, group_id, i, total, ds_max_raw, ds_chunk_size);
}
```

**3. Параллельные pump tasks:**

Каждая sub-session запускает свой pump task (tokio::spawn).
Все pump'ы пишут в один общий `streams: Arc<RwLock<HashMap<u16, StreamDownstream>>>`.
reorder_buf (BTreeMap<u32, Bytes>) в StreamDownstream уже собирает по seq_num.

**4. Upstream (Connect + Data):**

Connect отправляется через ОДНУ sub-session (первую или случайную).
Upstream Data — через любую sub-session (round-robin).

### Что НЕ менять

- WINDOW_SIZE=4, PIPELINE_WIDTH=3 — per sub-session, проверено
- BatchTracker, ACK — per sub-session
- TunnelPacket формат — seq_num уже глобальный per-stream
- Transport layer (DNS UDP/TCP/DoH/DoH+ECH)
- AdaptivePacer
- Existing tests

## Тестирование

### VPS доступ
- Сервер: `203.0.113.130`, SSH: `root` / `203.0.113.130`
- Клиент: `203.0.113.102`, SSH: `root` / `203.0.113.130`
- Домен: `tunnel.example.com`
- Cert: `/opt/slipstream-rust-plus/cert.pem` на сервере

### Сборка и деплой
```bash
cd /work/vpn/dnstt/slipstream-pure/dev/v1.8-windowed-pacing
cargo build --release -p sp-server -p sp-client

# Деплой сервера
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.130 'pkill -f sp-server'
sshpass -p '<REDACTED_PASSWORD>' scp target/release/sp-server root@203.0.113.130:/opt/slipstream-rust-plus/sp-server-v2
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.130 'cd /opt/slipstream-rust-plus && RUST_LOG=info ./sp-server-v2 --domain tunnel.example.com --dns-listen 203.0.113.130:53 --cert cert.pem --key key.pem &>/var/log/slipstream-v2.log &'

# Деплой клиента
sshpass -p '<REDACTED_PASSWORD>' scp target/release/sp-client root@203.0.113.102:/opt/slipstream-rust-plus/sp-client-v2

# Тест с 2 resolver'ами (1 sub-session, baseline)
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.102 'cd /opt/slipstream-rust-plus && RUST_LOG=info timeout 20 ./sp-client-v2 --domain tunnel.example.com --config resolvers-adaptive.toml --cert cert.pem --listen 127.0.0.1:7000 2>&1 | head -20'

# Speed test
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.102 'curl -s -x socks5://127.0.0.1:7000 --max-time 30 -o /dev/null -w "speed: %{speed_download} B/s\nsize: %{size_download}\ntime: %{time_total}s\n" http://httpbin.org/bytes/51200'
```

### Тест с N sub-sessions

Создать конфиг с 6+ resolver'ами для теста (Google, Cloudflare, Quad9):
```toml
[[resolver]]
name = "Google1"
ip = ["8.8.8.8"]
edns0_max = 4096

[[resolver]]
name = "Google2"
ip = ["8.8.4.4"]
edns0_max = 4096

[[resolver]]
name = "Cloudflare1"
ip = ["1.1.1.1"]
doh = "https://cloudflare-dns.com/dns-query"
edns0_max = 4096

[[resolver]]
name = "Cloudflare2"
ip = ["1.0.0.1"]
doh = "https://cloudflare-dns.com/dns-query"
edns0_max = 4096

[[resolver]]
name = "Quad9-1"
ip = ["9.9.9.9"]
edns0_max = 4096

[[resolver]]
name = "Quad9-2"
ip = ["149.112.112.112"]
edns0_max = 4096
```

6 resolver'ов / 3 на группу = 2 sub-sessions → ожидаем ~50 KB/s (2 × 25).

### Критерии успеха
1. `cargo check` — без ошибок
2. Без group (старый клиент) — работает как раньше (обратная совместимость)
3. С group из 2 sub-sessions — скорость ~2× от одной session
4. Сервер логирует: `session XXX group=YYY sub=0 total=2`
5. Сервер логирует: round-robin distribution по sub-sessions

## Порядок реализации

1. **sp-common/src/protocol.rs** — парсинг group/sub/total из Hello payload
2. **sp-server** — SessionGroup, маппинг, round-robin в stream_forward_task
3. **sp-client** — разбиение resolver'ов на группы, N pump tasks, общий reassembly
4. **Тест** — сборка, деплой, speed test на VPS
5. **Коммит** — НЕ упоминать AI/Claude в сообщении коммита

## Важно

- НЕ менять WINDOW_SIZE, PIPELINE_WIDTH, timeout — это проверено
- НЕ ломать обратную совместимость — без group= должно работать как раньше
- НЕ упоминать Claude/AI в коммитах
- Перед изменениями на VPS — бэкап текущих бинарников
- Тестировать на VPS, не локально
