# Fix Task 6: Оптимизация скорости — EDNS0 + Data Pump + Scanner

## Проект
`/home/hgff/slipstream-pure/`

## Текущее состояние
- HTTP/HTTPS работают
- Скорость: **3 KB/s** (нужно 200+ KB/s)
- 80 good paths, 18 резолверов
- Upstream chunk: 80 байт (QNAME encoding)
- Downstream chunk: 240 байт

## План — СТРОГО ПО ПОРЯДКУ

---

### Шаг 1: EDNS0 OPT encoding для upstream (80 → 1232 байт)

**Проблема:** Сейчас данные кодируются в DNS QNAME (subdomain labels, base32). Максимум ~80 байт на запрос.

**Решение:** Slipstream-rust-plus кодирует данные в EDNS0 OPT record RDATA (raw binary). Это даёт до 1232 байт на запрос — в 15 раз больше.

**Реализация:**

В sp-common/src/dns_codec.rs:
- Добавить `encode_upstream_edns0(data: &[u8]) -> Vec<u8>` — упаковать данные в EDNS0 OPT record
- Оставить старый QNAME encoding как fallback

В sp-transport:
- DNS-запросы должны включать EDNS0 OPT record с:
  - UDP payload size = 4096
  - Данные в OPT RDATA
- Question name = просто `<session_id>.tunnel.example.com` (без данных в subdomain)
- Данные идут в OPT record

В sp-server:
- При получении DNS-запроса: сначала проверить EDNS0 OPT RDATA на данные
- Если есть — декодировать оттуда
- Если нет — fallback на QNAME декодирование

Формат DNS-запроса (новый):
```
Question: <session_id>.tunnel.example.com TXT
Additional:
  OPT record:
    UDP payload size: 4096
    RDATA: [raw tunnel data, up to 1232 bytes]
```

**Тест после шага 1:** HTTP и HTTPS работают. Upstream chunk теперь ~1200 байт.

---

### Шаг 2: Data pump — непрерывный поток запросов

**Проблема:** Клиент ждёт ответ на каждый DNS-запрос перед отправкой следующего. При RTT 200мс это максимум 5 запросов/сек.

**Решение:** Не ждать ответа. Отправлять запросы непрерывно через разные пути. Ответы обрабатывать асинхронно.

**Реализация:**

В sp-client:
- Upstream: при чтении данных из SOCKS5, сразу отправлять DNS-запрос через `tokio::spawn` (fire-and-forget)
- Каждый запрос через другой good path (round-robin)
- Не ждать ответа — сразу читать следующую порцию и отправлять
- Ответы (downstream) обрабатывать в отдельной задаче
- Контроль: максимум 20-30 запросов in-flight одновременно

Polling:
- Держать постоянно 5-10 poll-запросов in-flight через разные good paths
- Когда получен ответ с данными — сразу отправить новый poll (data pump)
- Каждый ответ может нести до 3500 байт downstream

**Тест после шага 2:** Скорость > 50 KB/s.

---

### Шаг 3: Максимизация downstream (EDNS0 + TXT chunking)

**Проблема:** Downstream пакеты ограничены 240 байт. DNS-ответ может нести до 4096 байт (EDNS0).

**Решение:**

В sp-server:
- DNS-ответы включают EDNS0 OPT record с UDP payload size 4096
- Downstream данные в TXT record, до 3500 байт
- TXT record split на 255-byte strings (RFC requirement)
- Каждый DNS-ответ = до 3500 байт downstream данных

В sp-client:
- Парсить TXT records с 255-byte chunking
- Поддержка EDNS0 в ответах

**Тест после шага 3:** Скорость > 100 KB/s.

---

### Шаг 4: DNS scanner — найти больше резолверов

**Проблема:** Используем только 18 резолверов. Больше резолверов = больше параллельных путей = больше скорость.

**Решение:** Встроить сканер в sp-client или сделать отдельную утилиту.

**Реализация:**

Создать `crates/sp-scanner/`:
- Взять список публичных DNS-резолверов (1000+)
- Для каждого проверить:
  1. Отвечает ли на DNS-запросы (ping)
  2. Пересылает ли на наш сервер (NS delegation test)
  3. Возвращает ли TXT-ответы (downstream test)
  4. Поддерживает ли EDNS0 (payload size test: 512, 1232, 4096)
  5. Latency
- Результат: список рабочих резолверов с характеристиками
- Автоматически дополнять resolvers.toml

Источники резолверов для сканирования:
- https://public-dns.info/nameservers.txt (~7000 публичных DNS)
- Встроенный список из findns
- Ручное добавление

Формат вывода:
```toml
[[resolver]]
name = "discovered-1.2.3.4"
ip = ["1.2.3.4"]
edns_max = 4096
latency_ms = 45
```

**Тест после шага 4:** 50+ новых рабочих резолверов. Скорость > 150 KB/s.

---

### Шаг 5: conntrack tuning на сервере

На VPS 203.0.113.130:
```bash
sysctl -w net.netfilter.nf_conntrack_max=262144
sysctl -w net.netfilter.nf_conntrack_udp_timeout=15
sysctl -w net.netfilter.nf_conntrack_udp_timeout_stream=60
```

Сохранить в `/etc/sysctl.d/99-tunnel.conf`.

---

## Как тестировать

```bash
# Сборка:
cd /home/hgff/slipstream-pure && cargo build -p sp-client -p sp-server --release

# Деплой:
sshpass -p '<REDACTED_PASSWORD>' scp target/release/sp-server root@203.0.113.130:/opt/sp-server
sshpass -p '<REDACTED_PASSWORD>' scp target/release/sp-client root@203.0.113.181:/opt/sp-client

# Перезапуск:
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.130 "killall sp-server; cd /opt && nohup ./sp-server --domain tunnel.example.com --dns-listen 203.0.113.130:53 > /var/log/sp-server.log 2>&1 &"
sleep 2
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.181 "killall sp-client; cd /opt && nohup ./sp-client --domain tunnel.example.com --config resolvers.toml --insecure > /var/log/sp-client.log 2>&1 &"
sleep 8

# HTTP:
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.181 "curl -x socks5://127.0.0.1:7000 -s --max-time 30 http://ifconfig.me"

# HTTPS:
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.181 "curl -x socks5://127.0.0.1:7000 -s --max-time 30 https://ifconfig.me"

# Скорость (ГЛАВНЫЙ ТЕСТ):
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.181 "curl -x socks5://127.0.0.1:7000 -s --max-time 120 -w 'Downloaded: %{size_download} bytes\nTime: %{time_total}s\nSpeed: %{speed_download} bytes/s\n' -o /dev/null http://speedtest.tele2.net/1MB.zip"
```

Цель: **> 100 KB/s** (сейчас 3 KB/s)

## Порядок работы

1. Шаг 1 (EDNS0 upstream) → тест HTTP/HTTPS → тест скорости
2. Шаг 2 (data pump) → тест HTTP/HTTPS → тест скорости
3. Шаг 3 (EDNS0 downstream) → тест HTTP/HTTPS → тест скорости
4. Шаг 4 (scanner) — если есть время
5. Шаг 5 (conntrack) — быстро, сделать в любой момент

**НЕ ломай HTTP/HTTPS!** Тестируй после каждого шага. Если сломалось — откатывай.

## Важно

- НЕ менять resolvers.toml (кроме шага 4 — добавление новых)
- НЕ менять sp-multipath (good-path tracking работает)
- Собирать release: `cargo build --release`
- После успеха: `cargo build -p sp-client --release --target x86_64-pc-windows-gnu`
