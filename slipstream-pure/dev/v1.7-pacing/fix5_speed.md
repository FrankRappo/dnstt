# Fix Task 5: Увеличить скорость в 50-100 раз

## Проект
`/home/hgff/slipstream-pure/`

## Проблема

Текущая скорость: **3 KB/s**. Должно быть: **200-500 KB/s**. Отставание в 100 раз.

HTTP и HTTPS работают — функциональность ОК. Проблема только в скорости.

## Причины медленности

1. **Upstream chunk 80 байт** — в один DNS-запрос влезает только 80 байт данных. Нужно использовать максимум.
2. **Downstream chunk 240 байт** — DNS-ответ может нести до 4096 байт (EDNS0). Сейчас используем только 240.
3. **Последовательная отправка** — ждём ответ на каждый DNS-запрос перед отправкой следующего. Нужен пайплайнинг.
4. **Нет параллельных запросов** — 80 good paths, но используем их по одному.

## Что нужно сделать

### 1. Увеличить downstream до EDNS0 (4096 байт)

В sp-server:
- DNS-ответы должны использовать EDNS0 OPT record с payload size 4096
- Паковать максимум downstream данных в каждый ответ (до ~3500 байт полезных)
- Не бояться больших ответов — если резолвер обрежет, good-path tracking его отключит

В sp-transport:
- DNS-запросы должны включать EDNS0 OPT record (заявлять поддержку 4096 байт)

**Эффект:** downstream скорость вырастет в ~15 раз (240→3500 байт на ответ)

### 2. Пайплайнинг — отправлять запросы не дожидаясь ответов

В sp-client:
- Upstream: читать данные из SOCKS5 и сразу отправлять DNS-запросы, НЕ ждать ответа
- Использовать tokio::spawn для каждого DNS-запроса (fire-and-forget для upstream)
- Ответы обрабатывать асинхронно — downstream данные из ответов диспатчить по мере получения

**Эффект:** upstream latency уменьшится с N×RTT до 1×RTT

### 3. Параллельные запросы через множество путей

В sp-client:
- Отправлять до 10-20 DNS-запросов ОДНОВРЕМЕННО через разные good paths
- Для poll: отправлять через 5 путей параллельно, брать первый ответ с данными
- Для upstream data: разные чанки через разные пути параллельно

В sp-multipath:
- `send_parallel(queries: Vec<Query>)` — отправить пакет запросов через разные пути
- Round-robin по good paths для распределения нагрузки

**Эффект:** пропускная способность вырастет пропорционально количеству параллельных путей

### 4. Увеличить upstream chunk

Сейчас 80 байт из-за DNS name limit 253. Но можно больше:
- DNS TCP: нет лимита на размер имени, можно использовать DNS message до 65KB
- DoH: HTTP POST body может содержать полный DNS-пакет с большим именем
- Для DNS UDP: оставить 80 байт (лимит протокола)
- Для DNS TCP/DoH: увеличить до 500+ байт

В sp-transport:
- Добавить метод `max_upstream_size()` в trait Transport
- DNS UDP: 80 байт, DNS TCP: 500 байт, DoH: 500 байт

В sp-client:
- Выбирать chunk size в зависимости от транспорта пути
- Для TCP/DoH путей отправлять большие чанки

**Эффект:** upstream вырастет в 5-6 раз для TCP/DoH путей

### 5. Aggressive polling

Сейчас poll каждые 50-100мс. Нужно:
- Во время активной передачи: poll каждые 10мс через 5 good paths параллельно
- В idle: poll каждые 200мс через 1 путь
- Каждый poll = возможность получить до 3500 байт downstream (с EDNS0)

**Эффект:** downstream throughput = 3500 байт × 5 путей × 100 polls/сек = ~1.7 MB/s теоретический максимум

## Порядок реализации

1. EDNS0 в ответах сервера (самый большой эффект)
2. EDNS0 в запросах клиента
3. Пайплайнинг upstream
4. Параллельные polls через good paths
5. Увеличение upstream chunk для TCP/DoH
6. Aggressive polling interval

## Тестирование

```bash
# Сборка и деплой:
cd /home/hgff/slipstream-pure && cargo build -p sp-client -p sp-server --release
sshpass -p '<REDACTED_PASSWORD>' scp target/release/sp-server root@203.0.113.130:/opt/sp-server
sshpass -p '<REDACTED_PASSWORD>' scp target/release/sp-client root@203.0.113.181:/opt/sp-client

# Перезапуск:
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.130 "killall sp-server; cd /opt && nohup ./sp-server --domain tunnel.example.com --dns-listen 203.0.113.130:53 > /var/log/sp-server.log 2>&1 &"
sleep 2
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.181 "killall sp-client; cd /opt && nohup ./sp-client --domain tunnel.example.com --config resolvers.toml --insecure > /var/log/sp-client.log 2>&1 &"
sleep 8

# Тест HTTP (должен работать):
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.181 "curl -x socks5://127.0.0.1:7000 -s --max-time 30 http://ifconfig.me"

# Тест HTTPS (должен работать):
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.181 "curl -x socks5://127.0.0.1:7000 -s --max-time 30 https://ifconfig.me"

# Тест скорости (ГЛАВНЫЙ):
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.181 "curl -x socks5://127.0.0.1:7000 -s --max-time 120 -w 'Downloaded: %{size_download} bytes\nTime: %{time_total}s\nSpeed: %{speed_download} bytes/s\n' -o /dev/null http://speedtest.tele2.net/1MB.zip"

# Цель скорости: >100 KB/s (сейчас 3 KB/s)
```

## Важно

- НЕ сломай HTTP/HTTPS! Тестируй после каждого изменения
- НЕ меняй resolvers.toml
- Приоритет: EDNS0 downstream → пайплайнинг → параллельные polls
- После успеха: `cargo build -p sp-client --release --target x86_64-pc-windows-gnu`
