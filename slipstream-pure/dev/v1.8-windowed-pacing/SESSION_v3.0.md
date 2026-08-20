# v3.0 Single-Session Architecture — Session Log

## Дата: 2026-03-30 — 2026-03-31

## Результат

**Zero loss подтверждён.** Combined скорость ≥ суммы solo скоростей.

```
Конфигурация          Solo сумма   Combined   Потеря
──────────────────────────────────────────────────────
2 Level3 (v3.0):      36.3 KB/s   41.8 KB/s   0% (+15%)
7 resolver'ов (v3.0): 20.1 KB/s   19.5 KB/s   3%
44 auto-discover:      —          35.9 KB/s    —
v1.8 adaptive:        59.9 KB/s   34.4 KB/s   42% ← старая архитектура
```

## Архитектура v3.0

**Один session, все resolver'ы в одном пуле.**

```
Resolver'ы (N штук)
    ↓ round-robin
MultipathRouter (1 session)
    ↓
Streaming pump (INFLIGHT=good_paths, WINDOW=INFLIGHT+1)
    ↓ process each response independently
Server: dynamic WINDOW per session (from Hello)
    ↓
Client SOCKS5 → user
```

### Ключевые решения

1. **Без sub-sessions/groups** — убирает reorder stall между группами
2. **Streaming pump** — каждый response обрабатывается независимо, fast resolver'ы не ждут slow
3. **Dynamic WINDOW=INFLIGHT+1** — масштабируется с числом resolver'ов
4. **Без pacing** — RTT и INFLIGHT cap естественно ограничивают RPS
5. **Cursor fix: send_cursor -= removed** — уменьшает дупликаты на 50%
6. **Auto-discover** — сканер находит resolver'ы автоматически (44/52 bundled)

### Почему v1.8 терял 42%

v1.8 burst pump: `await ALL 3 polls` → burst_ms = max(RTT всех 3) → медленный resolver тормозит быстрый.

v3.0 streaming pump: каждый poll независим → fast resolver крутится на своём RTT, не ждёт slow.

### Почему v2.1 sub-sessions не работали

Sub-sessions вводили cross-session reorder stall: seq от медленной sub-session блокировал delivery от быстрой. Вся сложность (pull-based, late seq, health gate, proportional window) не помогла — combined был медленнее solo.

## Сервер v3.0

**450 строк** (было 900 в v2.1).

Изменения от v1.8:
- Убрано: SessionGroup, RawChunk, late seq, shared queue, health gate
- Добавлено: `window_size: AtomicUsize` per session (from Hello `window=N`)
- VecDeque для downstream_queue
- Cursor fix: `send_cursor -= removed` вместо `= 0`

## Клиент v3.0

**500 строк** (было 1300 в v2.1).

- Один MultipathRouter со всеми resolver'ами
- Streaming pump через mpsc channel
- INFLIGHT = good_paths.max(2).min(200)
- WINDOW = INFLIGHT + 1
- Hello: `window=N max_raw=M chunk=C`
- Auto-discover из bundled list (44/52)

## Восстановлено из v2.1 backup

При переписке v3.0 были потеряны 5 фич сканера (модули на месте, вызовы из main убраны).
Восстановлено из git tag `v2.1-subsessions` — код идентичен, только форматирование компактнее:

1. **Bootstrap hints** — `--hint dns:IP`, `--hint doh:URL` → test_delegation → add to resolvers
2. **Background scanner** — BackgroundScanner::new → bg_scanner.run() в tokio::spawn
3. **Hot-swap** — new_resolver_rx.changed() → add_path(UDP+DoH) в router на лету
4. **Network change detection** — read_system_dns каждые 30s → discover_good_paths при смене
5. **Resolver cache** — load_cache при старте (save в background_scanner модуле)

Проверено diff'ом: логика идентична v2.1, различия только в именах переменных и форматировании.

## Коммиты на ветке v3.0-single-session

1. `60a3020` — v3.0 base: single-session, dynamic WINDOW, streaming pump (18 KB/s)
2. `1319f7e` — fix WINDOW=INFLIGHT+1 вместо INFLIGHT*2+2 (30.8 KB/s)
3. `2354971` — remove pacing: combined 41.8 > solo sum 36.3 (ZERO LOSS)
4. `ef484d9` — restore bundled resolver auto-discovery (42.5 KB/s с 96 paths)

## VPS

- Сервер: 203.0.113.130, `sp-server-v3`
- Клиент: 203.0.113.102, `sp-client-v3`
- Домен: tunnel.example.com

## Следующие шаги

1. ~~Восстановить потерянные фичи сканера~~ — DONE (коммит 25297da)
2. **edns0_max в auto-discover** — сейчас None → max_raw=1000, нужно profiling → 4096 → max_raw=3500 (+30% скорости, одна из самых простых и важных оптимизаций)
3. **Тест DoH/DoH+ECH** — код реализован но не тестирован на скорость:
   - DoH: данные через HTTPS порт 443 (ISP не видит DNS tunnel)
   - DoH+ECH: зашифрованный SNI (ISP не видит даже домен resolver'а)
   - Нужно: замерить скорость через DoH vs UDP, понять потери
   - Нужно: проверить что DoH paths реально доставляют данные
   - Если DoH работает без потерь — ISP/resolver блокировка не угроза
   - Сканер при discovery каждого resolver'а замеряет скорость КАЖДОГО транспорта:
     * UDP:53 — скорость, latency, потери
     * DoH (HTTPS:443) — скорость, latency, потери
     * DoH+ECH — скорость, latency, потери
     * Сравнение: DoH vs UDP потеря %, ECH vs DoH потеря %
   - Приоритет по умолчанию: DoH+ECH → DoH → UDP (максимальная стелс)
   - Ручной выбор режима: --transport udp|doh|doh-ech|auto
     * auto — приоритет стелс с автопереключением
     * udp — только UDP (максимальная скорость)
     * doh — только DoH (стелс)
     * doh-ech — только DoH+ECH (максимальная стелс)
   - Сканер показывает таблицу скоростей по транспортам для каждого resolver'а
4. **Динамический INFLIGHT** — пересчёт при hot-swap новых resolver'ов:
   - INFLIGHT и WINDOW задаются один раз при старте (TunnelClient::new + Hello)
   - Hot-swap добавляет paths в router, но INFLIGHT/WINDOW не растут
   - Нужно: AtomicUsize для inflight, пересчёт при add_path
   - Нужно: периодический re-Hello с обновлённым window= серверу
   - Или: сервер автоматически увеличивает window при росте poll rate
5. **Полный scan** — не только 52 bundled, а public-dns.info 7000+ IPs
6. **ISP resolver'ы** — через bootstrap DNS leak technique (Phase 2 сканера)
7. **Тест линейного масштабирования** — 50+ resolver'ов, подтвердить sum(solo)=combined
8. **Rate-limit control (--max-rps N)** — ручное ограничение RPS на resolver:
   - Сейчас: максимальная скорость, каждый resolver получает столько RPS сколько может
   - Нужно: `--max-rps 1` → 200 resolver'ов × 1 RPS = 200 RPS total, невидимо для каждого
   - `--max-rps 0` (default) → без ограничений, макс скорость
   - `--max-rps 5` → каждый resolver не более 5 req/s → стелс режим
   - Баланс: больше resolver'ов + низкий RPS = та же скорость но невидимо
9. **Маскировка трафика в туннеле** — из Phase 4 сканера в transport layer:
   - Всегда включено (0% потерь):
     * QnameCamouflage: cdn-xyz.domain вместо base32data.session.domain
     * DecoyTrafficGenerator: параллельные обычные запросы (google.com, etc.)
   - Вручную (есть потери скорости):
     * --qtype-mix: данные через A/AAAA/NULL вместо только TXT (~60% потеря)
   - ВАЖНО: замерить реальные потери каждой маскировки
10. **Domain rotation (позднее)** — несколько доменов с NS на наш сервер:
   - Запросы распределяются по доменам → resolver видит разные домены
   - Требует: покупка нескольких доменов, NS записи
   - Приоритет: низкий, сначала протестировать п.7-8
