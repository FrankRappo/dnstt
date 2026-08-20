# Benchmark Results — 2026-03-25

## Полная таблица всех версий

| Версия | HTTPS (из 5) | Скорость (макс) | Примечание |
|---|---|---|---|
| **v1.0-EARLY (0901)** | **5/5 (100%)** | 0 B/s (зависает) | 617L сервер, 646L клиент |
| **v1.0-FINAL (1115)** | **4/5 (80%)** | 143 B/s | 703L сервер, 712L клиент |
| **v1.5-STABLE (1428)** | **3/5 (60%)** | **15 KB/s** | 513L сервер, 700L клиент. Лучший баланс |
| v1.5-PRE-RETRY (1537) | 2/5 (40%) | 15 KB/s | 513L сервер, 715L клиент |
| v1.5-CURRENT (1547) | 2/5 (40%) | 3.4 KB/s | 513L сервер, 732L клиент |
| v1.5-DEPLOYED (binaries/) | 2/5 (40%) | 3.4 KB/s | Предсобранные бинарники |
| v1.6-WINDOW | 2/5 (40%) | 7 KB/s | Sliding window + BatchTracker |
| v1.6-PARALLEL | 2/5 (40%) | 7.4 KB/s | + parallel initial send |
| orig slipstream-rust-plus | 0/5 | — | picoquic, не работает |

## Тенденция

```
HTTPS reliability:  v1.0-EARLY(100%) → v1.0-FINAL(80%) → v1.5-STABLE(60%) → v1.5+(40%)
Speed:              v1.0(0 B/s)      → v1.0(143 B/s)   → v1.5(15 KB/s)    → v1.6(7 KB/s)
```

Оптимизация скорости убила HTTPS reliability.
v1.5-STABLE — лучший баланс (60% + 15 KB/s).

## conntrack

Проверен на сервере 203.0.113.130:
- conntrack модуль НЕ загружен
- iptables пустые (ACCEPT all)
- НЕ является причиной проблем

## v1.7-PACING (новая версия)

Базируется на v1.5-STABLE. Изменение: pump `send_parallel_good` уменьшен с 10 до 3,
паузы увеличены с 5/20ms до 15/30ms.

| Версия | HTTPS | Скорость |
|---|---|---|
| **v1.7-PACING** | **4/5 (80%)** | **7-8 KB/s** |

Код: `/work/vpn/slipstream-pure-v1.7-pacing/`
Бинарники: `binaries/servers/sp-server-v17pacing`, `binaries/clients/sp-client-v17pacing`

### Причина улучшения
v1.5 флудил resolver'ы 10 параллельными запросами → rate-limiting → потери 40%.
v1.7: 3 параллельных → resolver'ы не лимитят → потери 20%.
v1.0: 1 последовательный → 0% потерь, но 0 скорость.

## Ключевой вывод

Оптимальный баланс reliability/speed зависит от количества параллельных poll'ов.
Больше parallel = больше скорость, но resolver'ы начинают rate-limit'ить.
v1.7 (3 parallel) — лучший баланс: 80% HTTPS + 8 KB/s.

## v1.8-WINDOWED-PACING (26 марта 2026)

Объединение sliding window (v1.6) + pacing (v1.7) + pipelining.
Сервер: DownstreamWindow (4 batch'а) + round-robin rotation.
Клиент: BatchTracker + отдельные poll'ы.

4 итерации pump'а:

| Итерация | Подход | HTTPS | Скорость | Проблема |
|---|---|---|---|---|
| iter1 | send_parallel_good(3) + rotation | 2/5 | 2 KB/s | 2 из 3 ответов теряются |
| iter2 | + 10ms pacing | 3/5 | 2 KB/s | Всё ещё 1 ответ из 3 |
| iter3 | 3 отдельных poll'а, await all, 10ms | 3/5 | 6.8 KB/s (short) | Pipelining подтверждён |
| iter4 | continuous spawner 10ms | 1/5 | 21 B/s | Флуд resolver'ов |
| iter5 | continuous spawner 40ms | 1/5 | 0 B/s | Слишком медленный spawner |
| **iter5b** | **burst 3 polls + 15/25ms** | **5/5** | **1.8 KB/s** | **Лучший HTTPS за всю историю** |
| iter6 | burst 3 + 5/15ms | 4/5 | 1.7 KB/s | Меньше pacing |
| iter7 | channel spawner 50ms | 1/5 | 0 B/s | 60 RPS → rate-limit |
| iter8 | channel + latest-ack | 1/5 | 0 B/s | ACK fix не помог |

### Single-resolver тесты (Google 8.8.8.8 только)

| Сервер | HTTPS | Скорость | Примечание |
|---|---|---|---|
| **MAX_RAW=500 W=4 + iter5b** | **3/3** | **4.6 KB/s** | Пайплайнинг идеален, 0 дублей |
| MAX_RAW=1000 W=4 + iter5b | ok | 4.65 KB/s | Большие batch'и не помогли |
| MAX_RAW=1000 W=8 + iter5b | ok | 3 KB/s | Больше окно = хуже |
| MAX_RAW=1000 W=4 + iter9 (channel 100ms) | 1/3 | 0 B/s | Channel spawner сломан |
| **MAX_RAW=1000 W=4 + iter9b (no sleep)** | **3/3** | **19 KB/s** | **РЕКОРД. Burst 3 + no pacing** |

**Ключевой результат iter3** — pipelining работает:
- 11 batch'ей за 0.8 секунды, out-of-order delivery, BatchTracker собирает
- batch_id: 3→2→4→1→8→5→7→6→9→11→10, ACK: 0→4→5→8→9→11

Код: `/work/vpn/slipstream-pure-v1.8-windowed-pacing/`
Бинарники: `binaries/servers/sp-server-v18wp`, `binaries/clients/sp-client-v18wp`
Бэкап: `slipstream-pure-v1.8-windowed-pacing-20260326-1845.tar.gz`

### Бинарники на серверах (v1.8)
- Сервер 203.0.113.130: `sp-server-v18wp` (DownstreamWindow + rotation)
- Клиент 203.0.113.181: `sp-client-v18wp` (iter1), `v18wp2` (iter2), `v18wp3` (iter3), `v18wp4` (iter4)

## Бэкапы с бинарниками на серверах

### Сервер 203.0.113.130 (/opt/)
- sp-server-v10early — v1.0-EARLY
- sp-server-v10 — v1.0-FINAL
- sp-server-v15stable — v1.5-STABLE
- sp-server-v15 — v1.5-DEPLOYED (binaries/)
- sp-server.bak — v1.6 промежуточный
- sp-server — v1.6-WINDOW финальный
- sp-server-v16p — v1.6-PARALLEL

### Клиент 203.0.113.181 (/root/)
- sp-client-v10early — v1.0-EARLY
- sp-client-v10 — v1.0-FINAL
- sp-client-v15stable — v1.5-STABLE
- sp-client-v15pre — v1.5-PRE-RETRY
- sp-client-v15cur — v1.5-CURRENT
- sp-client-orig — v1.5-DEPLOYED (binaries/)
- sp-client-v16 — v1.6-WINDOW
- sp-client-v16p — v1.6-PARALLEL

## Смешанные пары (несовместимые версии)

| Сервер | Клиент | HTTPS | Почему |
|---|---|---|---|
| v1.5 | v1.6 | 0/5 | Клиент ждёт batch_id prefix, сервер не шлёт |
| v1.6 | v1.5 | 0/5 | Сервер шлёт batch_id prefix, клиент не парсит |
| v1.5 | v1.5 | 2/5 | Совместимы |
| v1.6 | v1.6 | 1/5 | Совместимы |

## Ранние тесты (во время разработки, хронологический порядок)

Эти тесты делались в разное время и не сравнимы напрямую с бенчмарком выше.
DNS условия менялись в течение дня.

| # | Что тестировали | HTTPS | HTTP | Примечание |
|---|---|---|---|---|
| 1 | v1.5 + первый retry код | 3/10 | — | Первый тест retry |
| 2 | + parallel initial send | 3/10 | — | Без улучшения |
| 3 | Агент: 8 retry, 3s timeout | 3/10 | — | Агент работал 3 часа |
| 4 | + DsHashRing + MAX_RAW=2000 | 4/10 | 3/5 | MAX_RAW ухудшил HTTP |
| 5 | MAX_RAW откачен к 500 | 4/10 | — | |
| 6 | Sliding window (агент, до фикса) | 1/10 | 2/5 | Window не заполнялся |
| 7 | Мой фикс window fill + strip prefix | 2/10 | 3/5 | HTTP починен |
| 8 | Timeout 5s вместо 3s | 2/10 | — | Не помогло |
| 9 | Baseline: оригинал v1.5 srv+cli | 3/10 | — | DNS conditions ~30% |
| 10 | send_parallel_good_all upstream | 2/10 | — | Блокировал, хуже |
| 11 | send_parallel_good(3) upstream | 1/10 | — | Ещё хуже |
| 12 | Диагностика: 1 HTTPS запрос | 200 | — | Sliding window: 13 batch за 0.8с, zero gaps |
| 13 | Диагностика: fail case | 000 | — | 12 batch получены, upstream TLS Finished не дошёл |
| 14 | Оригинальный slipstream-rust-plus | 0/10 | 0/10 | Полностью не работает |

## Сырые данные тестов

- `raw_test_pairs.txt` — смешанные пары v1.5/v1.6
- `raw_test_bench.txt` — первый бенчмарк (4 версии со скоростью)
- `raw_test_remaining.txt` — все остальные версии + ретесты

## Скрипты тестирования
- `/work/vpn/slipstream-pure-dev/test_bench.sh` — первый бенчмарк (4 версии)
- `/tmp/test_pairs.sh` — тест пар
- `/tmp/test_remaining.sh` — оставшиеся версии + ретесты
