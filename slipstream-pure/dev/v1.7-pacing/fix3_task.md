# Fix Task 3: Восстановить HTTP, починить HTTPS

## Проект
`/home/hgff/slipstream-pure/` — DNS tunnel на чистом Rust.

## Текущее состояние

- HTTP **работал** через туннель (curl http://ifconfig.me → 203.0.113.130)
- Последние изменения (downstream path tracking) сломали HTTP
- HTTPS не работает — из 95 путей только 3-5 доставляют TXT-ответы, round-robin попадает на рабочий путь с вероятностью ~3%

## Что делать — СТРОГО ПО ПОРЯДКУ

### Шаг 1: Откатить downstream tracking, вернуть рабочий HTTP

Откати все изменения связанные с downstream path tracking. Верни код к состоянию когда HTTP работал стабильно. НЕ теряй рабочие фиксы:
- Lazy TCP connect — ОСТАВИТЬ
- Seq dedup (upstream) — ОСТАВИТЬ
- QNAME dedup (downstream) — ОСТАВИТЬ
- TCP_NODELAY — ОСТАВИТЬ
- Маленькие downstream пакеты 240 байт — ОСТАВИТЬ
- Inline polling — ОСТАВИТЬ

Убрать: aggressive downstream path deactivation.

**Тест:** `curl -x socks5://127.0.0.1:7000 -s --max-time 30 http://ifconfig.me` должен вернуть `203.0.113.130`

Не переходи к шагу 2 пока HTTP не работает.

### Шаг 2: Smart downstream tracking (без ложных деактиваций)

Проблема предыдущей реализации: пути деактивировались при Hello/Connect/Poll когда пустой ответ — это нормально.

Правильная логика:
- Пустой ответ при Hello — НОРМАЛЬНО, не failure
- Пустой ответ при Connect — НОРМАЛЬНО, не failure
- Пустой ответ при Poll когда нет активных stream — НОРМАЛЬНО, не failure
- Пустой ответ при Poll когда ЕСТЬ активный stream И сервер должен отвечать данными — ЭТО failure

Реализация:
- Добавь в MultipathRouter метод `report_downstream_success(path_id)` и `report_downstream_empty(path_id)`
- В клиенте: вызывать `report_downstream_success` только когда получен непустой TXT с реальными данными
- В клиенте: вызывать `report_downstream_empty` только когда ожидались данные но пришёл пустой ответ (т.е. после отправки Data, при active streams)
- Деактивация: только если downstream failure rate > 90% при минимум 10 попытках

**Тест:** HTTP по-прежнему работает.

### Шаг 3: Good-path tracking для HTTPS

Проблема HTTPS: TLS ServerHello ~3252 байт = ~14 DNS-ответов. При round-robin по 95 путям (из которых 3-5 рабочих) вероятность доставки каждого ~3%. 14 пакетов при 3% = невозможно.

Решение — good paths:
1. **Запоминать рабочие пути** — когда путь успешно доставил downstream данные, пометить его как "good"
2. **Polling только через good paths** — не round-robin по всем, а только через проверенные
3. **Параллельный poll** — отправлять poll через 3-5 good paths ОДНОВРЕМЕННО. Кто первый ответил с данными — берём его ответ
4. **Повторять poll** — пока не получим все downstream данные, продолжать поллить через good paths
5. **Fallback** — если good paths пустой список (начало работы), использовать все пути. Как только найдётся хоть один good — переключиться на него

Реализация в sp-multipath:
- `Vec<PathId>` good_paths — пути с подтверждённым downstream
- `send_to_good_paths(query)` — отправить через лучшие пути
- `send_parallel(query, count)` — отправить через N good paths параллельно, вернуть первый успешный ответ

Реализация в sp-client:
- При poll использовать `send_to_good_paths` вместо обычного `send`
- При получении данных — пометить путь как good
- Для HTTPS: поллить агрессивнее (каждые 20мс вместо 50мс) пока TLS handshake не завершится

**Тест:** `curl -x socks5://127.0.0.1:7000 -s --max-time 30 https://ifconfig.me` должен вернуть `203.0.113.130`

## Как тестировать

### Серверы:
```bash
# Сервер:
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.130 "killall sp-server; cd /opt && nohup ./sp-server --domain tunnel.example.com --dns-listen 203.0.113.130:53 > /var/log/sp-server.log 2>&1 &"

# Клиент:
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.181 "killall sp-client; cd /opt && nohup ./sp-client --domain tunnel.example.com --config resolvers.toml --insecure > /var/log/sp-client.log 2>&1 &"
```

### Загрузка бинарников:
```bash
cd /home/hgff/slipstream-pure && cargo build -p sp-client -p sp-server --release
sshpass -p '<REDACTED_PASSWORD>' scp target/release/sp-server root@203.0.113.130:/opt/sp-server
sshpass -p '<REDACTED_PASSWORD>' scp target/release/sp-client root@203.0.113.181:/opt/sp-client
```

### Тесты:
```bash
# HTTP (должен работать после шага 1):
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.181 "curl -x socks5://127.0.0.1:7000 -s --max-time 30 http://ifconfig.me"

# HTTPS (должен работать после шага 3):
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.181 "curl -x socks5://127.0.0.1:7000 -s --max-time 30 https://ifconfig.me"
```

Ожидаемый результат обоих: `203.0.113.130`

### Логи:
```bash
# Клиент:
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.181 "tail -30 /var/log/sp-client.log"

# Сервер:
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.130 "tail -30 /var/log/sp-server.log"
```

## Важно

- НЕ менять sp-transport
- НЕ удалять резолверы из resolvers.toml — оставить все 18
- Порядок: HTTP первый, HTTPS второй. Не ломай HTTP при починке HTTPS
- После успеха — собрать под Windows: `cargo build -p sp-client --release --target x86_64-pc-windows-gnu`
- Тестируй после КАЖДОГО шага
