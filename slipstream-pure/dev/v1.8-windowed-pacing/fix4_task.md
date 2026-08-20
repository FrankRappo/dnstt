# Fix Task 4: HTTPS — клиент слишком медленно отправляет данные

## Проект
`/home/hgff/slipstream-pure/`

## Текущее состояние

- **HTTP работает** — подтверждено, 203.0.113.130 за 0.57 секунды
- **HTTPS не работает** — TLS handshake timeout

## Диагностика HTTPS

Лог сервера:
```
09:31:03 stream 2 connected to 34.160.111.145:443
09:31:08 stream 2 forwarding 80 bytes seq 2     ← 5 СЕКУНД после Connect!
09:31:08 stream 2 forwarding 80 bytes seq 3
09:31:08 stream 2 forwarding 80 bytes seq 4
09:31:13 stream 2 destination closed connection (EOF)  ← destination устало ждать
09:31:13 stream 2 forwarding 80 bytes seq 5     ← слишком поздно
09:31:13 stream 2 forwarding 80 bytes seq 6
09:31:13 stream 2 TCP write error: Broken pipe
```

Лог клиента:
```
09:30:50 good path found: path-4 (1 total)       ← только 1 good path после HTTP
09:31:03 SOCKS5 CONNECT 34.160.111.145:443
09:31:13 stream 2 disconnect                      ← 10 секунд, ничего не получено
```

## Проблемы

### 1. 5 секунд между Connect и первым Data
TLS ClientHello ~300 байт = 4 пакета по 80 байт. Они должны уйти за <1 секунду. Но 5 секунд — потому что:
- Только 1 good path (path-4) после HTTP-теста
- Все DNS-запросы идут через этот один путь последовательно
- Каждый DNS round-trip ~500мс через резолвер
- 4 пакета × 500мс = 2 секунды минимум, а реально хуже

### 2. Destination закрывает idle соединение
ifconfig.me:443 ожидает TLS ClientHello в течение ~5 секунд. Lazy TCP connect открывает соединение при первом Data, но пока все 4 чанка дойдут — уже timeout.

### 3. Только 1 good path
Good paths заполняется медленно — только после успешного downstream с данными. При старте системы good paths пуст, всё идёт через random paths. Нужно быстрее находить рабочие пути.

## Что нужно исправить

### Фикс 1: Параллельная отправка upstream данных
Сейчас 4 чанка TLS ClientHello отправляются ПОСЛЕДОВАТЕЛЬНО через 1 good path. Нужно:
- Отправлять чанки ПАРАЛЛЕЛЬНО через РАЗНЫЕ пути
- Если good_paths > 1, каждый чанк идёт через свой путь
- Это сократит время отправки с 4×500мс = 2с до 1×500мс = 0.5с

### Фикс 2: Быстрый discovery good paths при старте
Сейчас good paths заполняется только при получении downstream данных. Нужно:
- При старте отправить Hello через ВСЕ пути параллельно
- Сервер должен отвечать на Hello с непустым TXT (echo)
- Пути которые вернули ответ — сразу good paths
- Это даст 5-10 good paths ещё до первого curl

### Фикс 3: Убрать lazy connect для HTTPS ИЛИ буферизировать данные
Варианты:
- **A)** Сервер буферизирует Data пакеты и открывает TCP только когда набрал достаточно (>200 байт для TLS ClientHello). Отправляет всё разом.
- **B)** Клиент буферизирует первые 500мс данных, отправляет всё одним пакетом (если влезает в DNS query) или несколькими быстро.
- **Рекомендую A)** — сервер ждёт 500мс или 300 байт (что раньше), потом открывает TCP и отправляет всё.

### Фикс 4: TCP connect раньше, не lazy
Альтернатива фиксу 3: убрать lazy connect, открывать TCP сразу при Connect но с TCP keepalive. Если destination закроет idle — переоткрыть при получении данных. Это проще.

## Приоритет фиксов

1. **Фикс 2** (быстрый discovery) — самый важный, даст больше good paths
2. **Фикс 1** (параллельная отправка) — ускорит upstream
3. **Фикс 3A** (буферизация на сервере) — надёжная доставка TLS ClientHello

## Как тестировать

```bash
# Сборка:
cd /home/hgff/slipstream-pure && cargo build -p sp-client -p sp-server --release

# Загрузка:
sshpass -p '<REDACTED_PASSWORD>' scp target/release/sp-server root@203.0.113.130:/opt/sp-server
sshpass -p '<REDACTED_PASSWORD>' scp target/release/sp-client root@203.0.113.181:/opt/sp-client

# Перезапуск:
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.130 "killall sp-server; cd /opt && nohup ./sp-server --domain tunnel.example.com --dns-listen 203.0.113.130:53 > /var/log/sp-server.log 2>&1 &"
sleep 2
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.181 "killall sp-client; cd /opt && nohup ./sp-client --domain tunnel.example.com --config resolvers.toml --insecure > /var/log/sp-client.log 2>&1 &"
sleep 5

# Тест HTTP (должен работать всегда):
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.181 "curl -x socks5://127.0.0.1:7000 -s --max-time 30 http://ifconfig.me"

# Тест HTTPS:
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.181 "curl -x socks5://127.0.0.1:7000 -s --max-time 60 https://ifconfig.me"

# Оба должны вернуть: 203.0.113.130
```

## Важно

- НЕ сломай HTTP! Тестируй HTTP после каждого изменения
- НЕ меняй sp-transport, resolvers.toml
- После успеха собери Windows: `cargo build -p sp-client --release --target x86_64-pc-windows-gnu`
