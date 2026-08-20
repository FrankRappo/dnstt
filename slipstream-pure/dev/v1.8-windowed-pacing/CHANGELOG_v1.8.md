# Slipstream Pure v1.8 — Windowed Pacing

## ПРИОРИТЕТ: Pull-based distribution (v2.1) — убрать head-of-line blocking

**Статус:** НЕ НАЧАТО — главный блокер масштабирования

**Проблема:** reorder_buf ждёт seq по порядку. При push-based distribution (сервер заранее назначает seq → sub-session) медленная sub-session создаёт gap → ВСЯ доставка стоит. С 5+ sub-sessions разных скоростей — 0.4 KB/s вместо 50+.

Это единственная причина, почему нельзя использовать неограниченное кол-во resolver'ов.

**Решение: demand-driven (pull-based) distribution**

```
Сейчас (push, сломано):                    Нужно (pull):
  Сервер назначает заранее:                  Общая очередь, sub-session БЕРЁТ когда готова:
  seq 1 → sub[0] (медленный)                sub[0] window freed → берёт seq 1
  seq 2 → sub[1] (быстрый)                  sub[1] window freed → берёт seq 2,3,4,5
  seq 3 → sub[2]                             sub[2] window freed → берёт seq 6,7
  ...                                        sub[1] window freed → берёт seq 8,9,10,11
  sub[0] тормозит → seq 1 не пришёл          ...
  → reorder_buf блокирует seq 2,3,4...       Быстрый sub[1] получает 80% данных
  → ВСЁ СТОИТ                                Медленный sub[0] получает 5% — не блокирует
```

**Изменения:**
1. Сервер: одна глобальная `downstream_queue` на SessionGroup (не per-sub-session)
2. `take_downstream_data` берёт chunks из общей очереди, seq назначается в момент выдачи
3. Быстрая sub-session дренирует window чаще → refill чаще → больше chunks
4. Медленная → мало seq в полёте → мало gaps → не блокирует reorder_buf

**Цель:** неограниченное кол-во resolver'ов, линейное масштабирование.
200 resolver'ов × 10 KB/s среднее = 2 MB/s.

---

## Обновление: 2026-03-29 — Adaptive Batch Size

Клиент вычисляет оптимальные downstream параметры (max_raw, chunk_size)
из EDNS0 profile каждого resolver'а и передаёт серверу через Hello.

- `edns0_max=4096` → `max_raw=3500, chunk=1500` (было 1000/480)
- Результат: **25 KB/s** (было 19 KB/s, +30%) на одном Google resolver'е
- Обратно совместимо: старые клиенты/серверы работают с дефолтами
- Коммит: `ec3cb7c`

## Обновление: 2026-03-30 — v2.0 N Sub-Streams

Один TCP-поток через N групп resolver'ов параллельно (striping).

### Серверные изменения
- `SessionGroup`: объединяет sub-sessions по group_id
- `stream_forward_task`: weighted block distribution по sub-sessions
- `AtomicUsize queue_len` + `AtomicU64 drain_count` — lock-free метрики
- Weighted blocks: каждая sub-session получает блок chunks пропорционально drain speed
- Обратная совместимость: без group= работает как v1.8

### Клиентские изменения
- Smart grouping: resolver'ы сортируются по score, snake distribution по группам
- N независимых pump tasks (каждый W=4/P=3)
- Общий reassembly через `streams` HashMap + reorder_buf
- `--resolvers-per-group N` — контроль размера групп
- Hello: `group=GID sub=N total=T`

### Эволюция distribution алгоритма
1. Round-robin: 6 KB/s — медленные sub-sessions блокируют reorder_buf
2. Fill-fastest (shortest queue): 19.7 KB/s — всё идёт в 1 быстрый resolver
3. Hybrid (RR idle + fill-fastest): 23.6 KB/s — лучше, но не все работают
4. **Weighted blocks (по drain speed): 28-36 KB/s** — все resolver'ы вносят вклад

### Результаты

| Конфигурация | Скорость |
|---|---|
| v1.8 один Google | 19 KB/s |
| + adaptive batch | 25 KB/s |
| 2 Google, 2 sub-streams | **37.6 KB/s** |
| 10 resolvers (mixed 3-25 KB/s), weighted | **28-36 KB/s** |

### Скорости отдельных resolver'ов (VPS 203.0.113.102)

| Resolver | IP | Скорость |
|---|---|---|
| Google1 | 8.8.8.8 | 25.5 KB/s |
| Cloudflare2 | 1.0.0.1 | 7.6 KB/s |
| Cloudflare1 | 1.1.1.1 | 6.0 KB/s |
| Quad9-1 | 9.9.9.9 | 5.1 KB/s |
| Quad9-2 | 149.112.112.112 | 4.5 KB/s |
| Google2 | 8.8.4.4 | 3.5 KB/s |

Для линейного масштабирования нужны resolver'ы с равной скоростью (ISP resolver'ы).

## Дата: 2026-03-26

## Суть
Объединение sliding window (v1.6) + pacing (v1.7) = **pipelining**.
Каждый poll получает РАЗНЫЙ batch из окна, а не дубль одного и того же.

## Что изменилось

### Сервер
- Заменён `pending_downstream` + `ds_batch_id` (v1.7, один pending batch) на `DownstreamWindow` (sliding window из 4 batch'ей)
- Каждый batch получает 4-байтный `batch_id` prefix (BE u32)
- **Round-robin `send_cursor`**: каждый poll возвращает следующий batch из окна
  - Poll A → batch_1, Poll B → batch_2, Poll C → batch_3
  - При обходе всего окна — ретрансмит с начала
- ACK от клиента: удаляются batch'и с `id <= client_ack`, окно дозаполняется
- На Connect: окно сбрасывается

### Клиент
- Добавлен `BatchTracker` (BTreeSet) вместо hash-based dedup (v1.7)
  - Отслеживает полученные batch_id'ы
  - Вычисляет contiguous ACK (наивысший batch_id без пропусков)
- `dispatch_downstream_with_batch` — разбирает 4-байтный batch_id prefix, трекает через BatchTracker
- Pacing от v1.7 сохранён: 3 parallel poll'а, pause 15/30ms
- Connection-level retry от v1.6: до 5 попыток Connect + initial data + poll
- Buffered initial data (TLS ClientHello) для retry

## Ожидаемый результат

### Было (v1.7):
- 3 poll'а → все получают ОДИН batch → 8 KB/s, 80% HTTPS

### Стало (v1.8):
- 3 poll'а → каждый получает СВОЙ batch → ожидание 20-30 KB/s, 80% HTTPS

### Теория:
```
v1.7: 1 batch / RTT = 500B / 50ms = 10 KB/s (реально 8)
v1.8: 3 batch / RTT = 1500B / 50ms = 30 KB/s
```

## Протокол

### Downstream flow:
```
Сервер: window = [batch_1, batch_2, batch_3, batch_4]
  Poll (ack=0) → return batch_1, cursor=1
  Poll (ack=0) → return batch_2, cursor=2
  Poll (ack=0) → return batch_3, cursor=3
  Poll (ack=3) → remove 1,2,3, fill [4,5,6,7], return batch_4, cursor=1
  Poll (ack=3) → return batch_5, cursor=2
```

### Формат batch:
```
[batch_id: 4 bytes BE u32][TunnelPacket data...]
```

### ACK в poll payload:
```
[ack: 4 bytes BE u32] — наивысший contiguous batch_id
```

## Совместимость
- v1.8 сервер + v1.8 клиент: OK
- v1.8 сервер + v1.7 клиент: НЕ РАБОТАЕТ (v1.7 клиент не понимает batch_id prefix)
- v1.7 сервер + v1.8 клиент: НЕ РАБОТАЕТ (v1.7 сервер не шлёт batch_id prefix)

## Файлы
- Сервер: `crates/sp-server/src/main.rs`
- Клиент: `crates/sp-client/src/main.rs`
- Бинарники: `binaries/servers/sp-server-v18wp`, `binaries/clients/sp-client-v18wp`

## Базируется на
- Сервер: v1.7 + DownstreamWindow из v1.6 + round-robin cursor (новое)
- Клиент: v1.7 + BatchTracker из v1.6 + connection-level retry из v1.6
