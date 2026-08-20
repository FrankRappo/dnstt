# Исследование v1.8: Оптимальная конфигурация DNS-туннеля

## Итог

**Оптимальная конфигурация (v10c / iter9b):**
- Сервер: MAX_RAW=1000, WINDOW_SIZE=4, chunk=480B, round-robin rotation
- Клиент: PIPELINE_WIDTH=3, timeout=500ms, burst + await-all, NO sleep
- Результат: **18-19 KB/s HTTPS на одном Google (8.8.8.8)**
- Пик: **35.8 KB/s raw** (по данным PUMP STATS)

## Почему именно эти параметры

### PIPELINE_WIDTH=3 (не 4, не 2)

**Тестировано:**
- WIDTH=3 → 3/3 HTTPS, 19 KB/s, 0 пропусков batch'ей
- WIDTH=4 → batch'и пропускаются, ACK застревает, 0 KB/s на stream

**Причина:** WINDOW_SIZE=4 означает 4 batch'а в серверном окне. При WIDTH=4 все 4 poll'а
получают разные batch'и (1,2,3,4). Если один poll теряется (timeout, resolver drop) —
клиент получает batch 1,2,4 но не 3. BatchTracker: pending=[4], ack застрял на 2.
Сервер видит ack=2, не удаляет batch 3,4 → окно не двигается → дубликаты.

При WIDTH=3 клиент получает 3 из 4 batch'ей. Если один потерян — только 2 получены,
но при следующем burst с обновлённым ACK сервер ротирует и отдаёт пропущенный.
4-й batch в окне служит **буфером для retransmit**.

**Правило: PIPELINE_WIDTH = WINDOW_SIZE - 1**

### timeout=500ms (не 200ms)

**Тестировано:**
- 500ms → burst_ms=35-40ms при данных, 500ms при пустых. 19 KB/s.
- 200ms → burst_ms=201ms ВСЕГДА (timeout блокирует). 1.8 KB/s.

**Причина:** DNS запрос через публичный resolver имеет RTT ~30-50ms в нормальных условиях,
но иногда до 300-400ms (resolver retransmit, TCP fallback, EDNS negotiation).
При timeout=200ms эти "длинные" ответы отбрасываются, хотя несут данные.
При timeout=500ms они успевают вернуться. Реальный burst_ms=35-40ms —
timeout не блокирует потому что все 3 poll'а возвращаются за ~50ms.

### NO sleep (не 15ms, не 25ms)

**Тестировано:**
- sleep 15/25ms → 4.6 KB/s (iter5b на single Google)
- sleep 5/15ms → 1.7 KB/s (iter6)
- NO sleep → **19 KB/s** (iter9b)

**Причина:** RTT (~50ms) **сам** является natural pacing. Цикл:
1. Отправить 3 poll'а (мгновенно, ~0ms)
2. Await all 3 ответов (~40-60ms, зависит от RTT)
3. Обработать ответы (~1ms)
4. Повторить

Без sleep: ~50ms на цикл = ~60 RPS на один resolver.
С sleep 15ms: ~65ms = ~46 RPS. Разница ×1.3 в RPS, но ×4 в скорости.
Почему ×4 а не ×1.3? Потому что sleep добавляется ПОСЛЕ обработки,
давая серверу время дренировать данные из downstream queue между burst'ами.
Без sleep следующий burst попадает в окно когда данные ещё есть → больше new,
меньше empty.

### MAX_RAW=1000 (не 500, не 1200)

**Тестировано:**
- MAX_RAW=500 → batch 502B, скорость 4.6 KB/s (с sleep), 19 KB/s (без sleep)
- MAX_RAW=1000 → batch 982B, скорость 4.65 KB/s (с sleep), 18 KB/s (без sleep)
- MAX_RAW=1200 → не тестировано (риск обрезки промежуточными сетями)

**Вывод:** MAX_RAW=500 vs 1000 почти не влияет на скорость при NO sleep.
Bottleneck не в размере batch'а, а в частоте burst'ов.
Оставлен 1000 как задел — при адаптивном batch size resolver'ы с EDNS0
смогут использовать полный объём.

### WINDOW_SIZE=4 (не 8)

**Тестировано:**
- WINDOW=4 → стабильная ротация, 0 пропусков с WIDTH=3
- WINDOW=8 → batch'и приходят с большим разбросом (11 раньше 9), pending растёт, ACK блокируется

**Причина:** с WINDOW=8 и WIDTH=3 клиент получает batch'и 1,5,3 (rotation циклит через 8).
Пропущены 2,4 → ack застрял на 1. С WINDOW=4 и WIDTH=3 — batch'и 1,3,2 или 2,1,3.
Максимальный gap = 1, быстро закрывается.

## Анализ сбоя (speed test 0 B/s)

### Что произошло
Первый speed test на v10c (stream-bytes/50000) вернул 0 B/s за 8.2 секунды.
Второй тест: 9.7 KB/s. Третий: 18.3 KB/s.

### Вероятные причины

**1. Idle timeout на downstream writer (8 секунд)**
Клиентский write_handle имеет `tokio::time::timeout(8s, downstream_rx.recv())`.
Если между TLS handshake и первыми данными от httpbin проходит >8 секунд — соединение закрывается.

Flow при stream-bytes:
```
curl → SOCKS5 → DNS tunnel → сервер → TCP к httpbin.org:443
                                      → TLS handshake (OK)
                                      → HTTP GET /stream-bytes/50000
                                      → httpbin начинает стримить
                                      → сервер читает chunks → downstream queue
                                      → poll'ы забирают batch'и → клиент
```

Если httpbin.org медленно начинает стримить (cold start, CDN edge selection),
между завершением TLS и первым chunk может пройти >8 секунд.

**2. DNS resolver temporary rate-limit**
Google может кратковременно дропить ответы после серии быстрых запросов
(TLS handshake генерирует ~40 batch'ей за 1 секунду). После "остывания"
следующий тест проходит нормально.

**3. Server-side downstream queue drain**
Между двумя тестами (HTTPS GET + stream-bytes) pump продолжает poll'ить.
Если предыдущая сессия не полностью дренировала очередь, новая сессия
может получить мусорные данные.

### Рекомендация
Увеличить idle timeout с 8s до 15s для длинных stream'ов.
Это не повлияет на HTTPS handshake (который завершается за ~2s),
но позволит медленным upstream'ам начать передачу.

## PUMP STATS — расшифровка

```
PUMP [2.0s]: sent=120 data=54 new=48 dup=6 empty=66 timeout=0 | ack=47 bursts=40 burst_ms=41 | 59 RPS, 28.2 KB/s raw
```

| Поле | Значение | Описание |
|---|---|---|
| sent=120 | 120 poll'ов за 2s | 60 RPS |
| data=54 | 54 вернули данные | 45% с данными |
| new=48 | 48 уникальных batch'ей | 40% полезных |
| dup=6 | 6 дубликатов | 5% дублей (отлично) |
| empty=66 | 66 пустых ответов | 55% пустых (queue дренирован) |
| timeout=0 | 0 таймаутов | Все poll'ы вернулись за <500ms |
| ack=47 | contiguous ACK=47 | 47 batch'ей доставлены без пропусков |
| bursts=40 | 40 burst'ов за 2s | 20 burst/s |
| burst_ms=41 | последний burst 41ms | = RTT (идеально) |
| 59 RPS | фактический RPS | 3 poll × 20 burst/s |
| 28.2 KB/s raw | пропускная способность | до декомпрессии |

### Идеальный burst:
```
burst_ms=35-41ms — все 3 poll'а вернулись за один RTT
new/data ≈ 90% — почти все ответы с данными уникальны
dup ≈ 10% — минимум дублей
timeout=0 — все в пределах 500ms
```

### Проблемный burst:
```
burst_ms=500+ — один poll таймаутил, заблокировал весь burst
dup > 50% — ACK не обновился, сервер шлёт те же batch'и
empty > 80% — нет данных в downstream queue
```

## Масштабирование

При оптимальной конфигурации на одном resolver'е:
- 1 Google: **19 KB/s** (подтверждено)
- 10 resolver'ов: ~190 KB/s (теоретически)
- 50 resolver'ов: ~950 KB/s (~1 MB/s)
- 200 resolver'ов: ~3.8 MB/s

Линейное масштабирование, потому что каждый resolver — независимый канал
с собственным rate-limit'ом.

**Следующие шаги:**
1. Адаптивный batch size (per-resolver MAX_RAW)
2. DNS-сканер (найти 200+ resolver'ов)
3. Тестирование multi-resolver с текущей конфигурацией

## Тесты оптимизации (v11 серия, 26 марта)

Попытки увеличить скорость сверх 19 KB/s на одном Google resolver'е.

| Изменение | Версия | Скорость | Результат |
|---|---|---|---|
| chunk=1024 + WINDOW=6 | v11 | 1.1 KB/s | СЛОМАНО. W=6 → gaps в batch_id → ACK застрял |
| chunk=1024 + WINDOW=4 | v11b | 12 KB/s | Хуже. Меньше packets в queue → больше empty polls |
| chunk=480 + WINDOW=4 (оригинал) | v11c | **21.4 KB/s** | Лучший результат. Совпадает с iter9b |

### Почему chunk=1024 хуже

С chunk=480: httpbin отдаёт 50KB → 104 TunnelPacket'а по ~480B → queue быстро заполняется →
window всегда полный → poll'ы получают данные.

С chunk=1024: те же 50KB → 49 TunnelPacket'ов по ~1024B → queue заполняется медленнее →
window часто полупустой → больше empty poll'ов.

При MAX_RAW=1000 один batch вмещает 2 packet'а по 480B (960B < 1000) или 1 packet по 1024B.
С chunk=480 batch несёт больше данных (2×480=960B) чем с chunk=1024 (1×1024, но overhead TunnelPacket
делает encoded size >1000 → 1 packet на batch).

### Почему WINDOW=6 и WINDOW=8 ломаются

С PIPELINE=3 и WINDOW=4: rotation через 4 batch'а, клиент получает 3 из 4.
Пропущенный batch — всегда следующий в ротации, получается при retransmit.

С PIPELINE=3 и WINDOW=6: rotation через 6 batch'ей. Клиент получает batch 1,3,5 (через один).
Batch 2,4 пропущены → ACK=1, pending=[3,5]. Нужны ещё 2 burst'а чтобы получить 2 и 4.
За это время сервер заполняет окно до [1,2,3,4,5,6] → ещё больше разброс.

**Вывод: WINDOW=4 + PIPELINE=3 — математически оптимальная пара.**

### Потолок одного resolver'а

19-21 KB/s — это потолок для Google 8.8.8.8 при текущей архитектуре:
- ~60 RPS (3 poll × 20 burst/s)
- ~40% poll'ов с данными, ~10% дубли
- ~48 new batch'ей / 2s × 1000B = ~24 KB/s raw → ~19 KB/s после overhead

Для увеличения скорости нужны **дополнительные resolver'ы**.

## Adaptive Batch Size (29 марта)

Реализован адаптивный размер downstream batch'ей на основе EDNS0 profile от сканера.

### Что сделано

Клиент вычисляет оптимальные `max_raw` и `chunk_size` для каждого resolver'а
из данных сканера (`edns0_max`, `rate_limit_rps`) и передаёт серверу через Hello.

| Компонент | Изменение |
|---|---|
| `ResolverConfig` / `ResolverEndpoint` | Добавлены поля `edns0_max`, `rate_limit_rps` |
| `config.rs` | `optimal_max_raw()` и `optimal_chunk_size()` из EDNS0 profile |
| `protocol.rs` | `new_hello_adaptive()` — передаёт max_raw/chunk в Hello payload |
| `sp-server Session` | Per-session `max_raw` / `chunk_size` (было захардкожено 1000/480) |
| `resolver_cache.rs` | Сохраняет/загружает edns0_max и rate_limit_rps |

### Формулы

```
optimal_max_raw = (edns0_max - 100 DNS overhead) - TXT length prefixes - 4B batch_id
                  clamp(200, 3500)

optimal_chunk_size = (max_raw - 18B TunnelPacket overhead×2) / 2
                     clamp(200, 1500)
```

При `edns0_max=4096`: max_raw=3500, chunk=1500 (было 1000/480).

### Результаты

| Конфигурация | MAX_RAW | chunk | Скорость | Прир��ст |
|---|---|---|---|---|
| v1.8 iter9b (хардкод) | 1000 | 480 | 19 KB/s | — |
| **adaptive (edns0=4096)** | **3500** | **1500** | **25 KB/s** | **+30%** |

Подтверждено на VPS: сервер 203.0.113.130, клиент 203.0.113.102.
Коммит: `ec3cb7c`.

### Обратная совместимость

- Клиент без adaptive → сервер использует дефолт 1000/480
- Старый сервер → игнорирует неизвестные поля в Hello payload
- `edns0_max = None` → fallback на 1000 (текущий хардкод)

### DoH HTTP/2 pipelining — уже работает

`DohTransport` использует `reqwest::Client` который по умолчанию включает HTTP/2.
Один `client` переиспользуется для всех запросов → все POST'ы идут по одному
TLS-соединению с мультиплексированием. Burst из 3 poll'ов через DoH path уже
идёт пайплайном. Отдельная задача не нужна.

## N Sub-Streams v2.0 (29-30 марта)

Реализован striping downstream через N групп resolver'ов.

### Что сделано
- Сервер: SessionGroup, round-robin distribution по sub-sessions
- Клиент: разбиение resolver'ов на группы, N параллельных pump tasks
- Протокол: group=GID sub=N total=T в Hello payload
- Chunked round-robin: CHUNKS_PER_SUB=4 (4 seq подряд в одну sub-session)

### Проблема скорости (не решена)

С 2 sub-sessions (6 resolver'ов): **6-7 KB/s** вместо ожидаемых 50 KB/s.
Baseline (1 сессия, 2 resolver'а): **27 KB/s** — работает нормально.

**Причина:** неравномерные группы resolver'ов.
- sub[0] (Google+Cloudflare): 7 RPS, timeout'ы 500ms, burst_ms=500
- sub[1] (Cloudflare+Quad9): 25 RPS, 0 timeout'ов, burst_ms=32-74

Медленная sub-session блокирует reorder_buf — клиент ждёт seq от sub[0]
чтобы доставить уже полученные seq от sub[1].

### Итерация 2: Smart grouping + fill-fastest (30 марта)

Реализовано:
- Клиент: сортировка resolver'ов по score (edns0_max × rate_limit_rps), snake distribution
- Сервер: fill-fastest — каждый chunk → sub-session с самой короткой очередью

### Бенчмарк отдельных resolver'ов (с клиентского VPS 203.0.113.102)

| Resolver | IP | Скорость |
|---|---|---|
| **Google1** | 8.8.8.8 | **25.5 KB/s** |
| Cloudflare2 | 1.0.0.1 | 7.6 KB/s |
| Cloudflare1 | 1.1.1.1 | 6.0 KB/s |
| Quad9-1 | 9.9.9.9 | 5.1 KB/s |
| Quad9-2 | 149.112.112.112 | 4.5 KB/s |
| Google2 | 8.8.4.4 | 3.5 KB/s |

Только Google 8.8.8.8 быстрый с этого VPS. Остальные rate-limit'ят.

### Результаты N sub-streams

| Конфигурация | Скорость | Статус |
|---|---|---|
| Google1 один | 25 KB/s | baseline |
| 2 Google, 1 session | 21 KB/s | baseline |
| **2 Google, 2 sub-sessions** | **37.6 KB/s** | **+50%, работает** |
| 10 resolver'ов, 5 groups | 0.5 KB/s | сломано |
| 10 resolver'ов, 10 groups | 0.4 KB/s | сломано |

### Вывод

2 sub-sessions с fill-fastest — работают, дают +50%.
5+ sub-sessions — ломаются. Вероятные причины:
- Mutex contention при fill-fastest с 5-10 sub-sessions
- Медленные sub-sessions накапливают данные в reorder_buf → timeout
- Не все sub-sessions успевают зарегистрироваться до Connect

### Следующие шаги

#### ПРИОРИТЕТ 1: Убрать head-of-line blocking в reorder_buf

**Проблема:** reorder_buf ждёт seq по порядку. Медленная sub-session блокирует ВСЮ доставку.
При 5+ sub-sessions с разными скоростями (Google 25 KB/s vs Quad9 4.5 KB/s) — катастрофа.
Это главный блокер для масштабирования на неограниченное кол-во resolver'ов.

```
Сейчас (push-based, сломано при разных скоростях):
  Сервер заранее назначает: seq 1→sub[0], seq 2→sub[1], seq 3→sub[2]
  sub[0] медленный → seq 1 не приходит → seq 2,3 лежат в reorder_buf → стоим

Нужно (pull-based / demand-driven):
  Сервер держит ОДНУ глобальную очередь chunks
  Sub-session запрашивает следующий chunk КОГДА ГОТОВА (window освободился)
  Быстрый resolver получает 10 chunks пока медленный получает 1
  Медленный НЕ блокирует быстрых — у него просто мало seq в полёте
```

**Решение: Pull-based distribution**

1. Сервер: одна глобальная `downstream_queue` для всей SessionGroup
2. Каждая sub-session при `take_downstream_data` (window refill) берёт chunks из общей очереди
3. Seq назначается В МОМЕНТ выдачи, не заранее
4. Быстрая sub-session дренирует window быстрее → чаще refill → получает больше chunks
5. Медленная sub-session получает меньше chunks → меньше seq в полёте → меньше gaps в reorder_buf
6. reorder_buf: gaps пропорциональны скорости sub-session, а не фиксированы round-robin

**Ожидаемый эффект:**
- 50 resolver'ов (разные скорости 4-25 KB/s) → каждый вносит что может, никто не блокирует
- Линейное масштабирование: N resolver'ов × средняя_скорость ≈ суммарная скорость
- 200 resolver'ов × 10 KB/s среднее = **2 MB/s** (без head-of-line blocking)

#### 2. ISP resolver'ы через сканер — потенциально 200+ rps, равные скорости
#### 3. Тестировать днём — resolver'ы стабильнее, меньше timeout'ов
#### 4. Proactive rate limiting — rate_limit_rps из profile → ограничение RPS в pacer'е

## Файлы

| Файл | Описание |
|---|---|
| `sp-client-v18-v10c` | Клиент v10c (PIPELINE=3, 500ms, no sleep, logging) |
| `sp-client-v18-iter9b` | Клиент iter9b (то же без logging) |
| `sp-server-v18-1k-w4` | Сервер (MAX_RAW=1000, WINDOW=4, chunk=480) |
| `resolvers-single.toml` | Конфиг: только Google 8.8.8.8 |
| Бэкап iter9b | `slipstream-pure-v1.8-iter9b-19kbs-20260326-2053.tar.gz` |
| Бэкап v10c | `slipstream-pure-v1.8-v10c-OPTIMAL-20260326-2133.tar.gz` |
| Бэкап v11 FINAL | `slipstream-pure-v1.8-v11-FINAL-20260326-2155.tar.gz` |
| `sp-server-v18-v11c` | Сервер v11c (W=4, chunk=480, MAX_RAW=1000, ds logging) |
