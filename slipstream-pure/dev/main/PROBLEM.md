# Проблема: DNS delivery ~60-70% убивает HTTPS

## Суть

HTTPS через DNS-туннель работает только ~30% (3/10 запросов).
HTTP работает лучше (~80-100%) потому что не требует полного TLS handshake.

## Что мы доказали

### Протокол работает правильно
Sliding window downstream доставляет 13 batch'ей за 0.8 секунды без единого gap:
```
pump: recv batch=1  ack_now=1  pending=[]
pump: recv batch=2  ack_now=2  pending=[]
...
pump: recv batch=13 ack_now=13 pending=[]
```

### Проблема — DNS transport, не наш код
Оригинальный бинарник v1.5 (без наших изменений) показывает тот же ~30% HTTPS.
Значит баг не в коде, а в среде.

## Два failure mode

### Mode 1: Initial downstream не приходит (~40% попыток)
- Клиент отправляет Connect + ClientHello через DNS
- Сервер НЕ получает (или получает, но ответ не доходит)
- Pump poll'ит 5 секунд — пусто
- Retry создаёт новый stream, иногда помогает, иногда нет

### Mode 2: Downstream пришёл, но upstream стоп (~20%)
- Sliding window доставляет ServerHello (12+ batch'ей)
- curl отправляет TLS Finished через upstream
- Upstream data НЕ доходит до сервера
- Сервер логирует: `queue=0, returning=None` — нет данных от httpbin
- curl висит 30 секунд и таймаутит

## Серверные логи подтверждают
При failure mode 2:
```
ds: ack=12 win_before=[] after_ack=0 queue=0 new=[] returning=None
ds: ack=12 win_before=[] after_ack=0 queue=0 new=[] returning=None
```
Сервер отдал 12 batch'ей, клиент ACK'нул, очередь пуста.
httpbin не отвечает потому что не получил HTTP запрос от сервера.
А сервер не получил TLS Finished от клиента через upstream DNS.

## Возможные причины

### 1. Resolver rate-limiting
Pump шлёт 15 параллельных poll'ов через `send_parallel_good`.
Upstream шлёт через `send_via_good_paths` с 5 retry.
Суммарно: десятки DNS-запросов в секунду к одному authoritative серверу.
Resolver'ы (Google, Cloudflare) могут rate-limit'ить.

### 2. conntrack overflow на сервере
ROADMAP v1.5 упоминает conntrack tuning (nf_conntrack_max, udp_timeout).
Если conntrack таблица переполнена — DNS-ответы дропаются.
**НЕ ПРОВЕРЕНО.**

### 3. Мёртвые good paths
При старте 75-82 good paths. Но проверка одноразовая (Hello echo).
За время работы paths могут деградировать. Reactivation каждые 60 секунд,
но не проверяет реальную delivery rate.

### 4. DNS response truncation
Downstream batch ~500 байт → compress → base64 → ~670 байт в TXT record.
Если resolver обрезает ответ > 512 байт → данные потеряны.
**НЕ ПРОВЕРЕНО.**

## Что нужно сделать

### Шаг 1: conntrack tuning (5 минут, сервер)
```bash
ssh root@203.0.113.130
sysctl net.netfilter.nf_conntrack_max
sysctl net.netfilter.nf_conntrack_count
# Если count близок к max — увеличить:
sysctl -w net.netfilter.nf_conntrack_max=262144
sysctl -w net.netfilter.nf_conntrack_udp_timeout=15
sysctl -w net.netfilter.nf_conntrack_udp_timeout_stream=60
```

### Шаг 2: Per-resolver диагностика (30 минут)
Написать скрипт: для каждого resolver'а отправить 10 DNS-запросов
к нашему серверу, замерить success rate и RTT.
Выявить мёртвые/медленные resolver'ы.

### Шаг 3: Проверить DNS response size
На сервере: залогировать размер DNS-ответов.
Если > 512 байт — уменьшить MAX_RAW или проверить EDNS0 support resolver'ов.

## Файлы

| Файл | Описание |
|---|---|
| `sp-server-main-v1.6.rs` | Копия серверного кода (526 строк) |
| `sp-client-main-v1.6.rs` | Копия клиентского кода (833 строки) |
| `SESSION_2025-03-25.md` | Подробный лог сессии |
| `PLAN_v1.6.md` | Архитектурный план (sliding window + FEC) |
| `PROBLEM.md` | Этот файл |
