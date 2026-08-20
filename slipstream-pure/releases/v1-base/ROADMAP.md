# Slipstream Pure — Roadmap

## Версия 1.0 (ТЕКУЩАЯ — ГОТОВА)
Базовый DNS-туннель на чистом Rust.

- [x] DNS encoding (QNAME base32)
- [x] 4 транспорта: DNS UDP, TCP, DoH, DoH+ECH
- [x] 18 резолверов, 80 good paths
- [x] Multipath без лимита путей
- [x] Good-path tracking
- [x] Adaptive pacing
- [x] Stream multiplexing
- [x] Smart downstream tracking
- [x] HTTP/HTTPS работает
- [x] Windows .exe
- [x] Скорость: 3 KB/s

---

## Версия 1.5 — Оптимизация скорости
**Цель:** 3 KB/s → 100-200 KB/s

### Шаг 1: EDNS0 OPT encoding upstream
- Данные в EDNS0 OPT RDATA вместо QNAME subdomains
- 1232 байт на запрос вместо 80
- Fallback на QNAME encoding для несовместимых резолверов
- **Ожидание:** 3 → 45 KB/s

### Шаг 2: Data pump
- Fire-and-forget upstream (не ждать ответ)
- 20-30 запросов in-flight одновременно через разные good paths
- Постоянно 5-10 poll-запросов in-flight для downstream
- При получении ответа — сразу новый запрос
- **Ожидание:** 45 → 100 KB/s

### Шаг 3: EDNS0 downstream
- DNS-ответы с EDNS0 UDP payload 4096
- TXT records с 255-byte chunking, до 3500 байт на ответ
- **Ожидание:** 100 → 150 KB/s

### Шаг 4: conntrack tuning
- nf_conntrack_max=262144
- udp_timeout=15
- udp_timeout_stream=60
- **Ожидание:** убрать потери пакетов

---

## Версия 2.0 — Автопоиск резолверов (DNS Scanner)
**Цель:** 18 резолверов → 200+, встроенный в клиент

### Шаг 1: Встроенный сканер
- Загрузка публичных списков DNS (~7000 IP)
- Источники: public-dns.info, bundled list
- Параллельное тестирование (50 workers)

### Шаг 2: Многоэтапная проверка резолверов
- Ping (отвечает ли)
- NS delegation test (пересылает ли на наш сервер)
- TXT response test (возвращает ли данные)
- EDNS0 payload size test (512, 1232, 4096)
- Latency замер

### Шаг 3: Автоматическая интеграция
- Фоновый скан при старте клиента
- Переодический ресканн каждые 10 минут
- Найденные рабочие — автоматически в good paths
- Ранжирование по latency + EDNS0 support + reliability

### Шаг 4: Persistent cache
- Сохранять найденные резолверы в файл
- При перезапуске — загружать кэш + досканивать новые
- TTL для записей (перепроверять через 1 час)

**Ожидание:** 200+ рабочих резолверов, скорость × 2-3

---

## Версия 3.0 — QUIC протокол (quinn)
**Цель:** 200 KB/s → 500+ KB/s, промышленное качество

### Шаг 1: Интеграция quinn
- Заменить самописный TunnelPacket протокол на quinn QUIC
- QUIC пакеты кодируются в DNS-запросы/ответы
- quinn управляет: flow control, retransmission, ordering, congestion

### Шаг 2: QUIC multipath
- Каждый резолвер = отдельный QUIC path
- quinn native multipath (если поддерживается) или ручная маршрутизация
- Per-path congestion control: BBR для быстрых, cubic для медленных

### Шаг 3: QUIC streams
- Каждое SOCKS5 соединение = QUIC stream
- Нативный stream multiplexing от quinn
- Убрать самописный stream_id/seq/reorder

### Шаг 4: Оптимизация
- Pacing-aware send rate (QPS из QUIC congestion window)
- Burst sending (10 пакетов за iteration)
- Single-threaded tokio runtime
- Zero-copy DNS encoding

**Ожидание:** 300-500 KB/s, стабильность промышленного уровня

---

## Версия 4.0 — Мобильные клиенты
**Цель:** Android, iOS

### Android
- Сборка: `cargo build --target aarch64-linux-android`
- GUI: Flutter или Jetpack Compose обёртка
- VPN mode через Android VpnService API
- Или Termux-совместимый CLI

### iOS
- Сборка: `cargo build --target aarch64-apple-ios`
- GUI: Swift обёртка
- NetworkExtension для VPN mode
- Или standalone app

---

## Версия 5.0 — Продвинутые фичи
- Domain fronting через CDN
- ECH (Encrypted Client Hello) для всех DoH путей
- Стеганография (данные внутри обычных HTTPS-запросов)
- P2P relay (клиенты помогают друг другу)
- Tor-style onion routing через DNS
- Web-интерфейс управления

---

## Сравнение итоговое

| | dnstt | Slipstream | Наш v1 | Наш v3 (цель) |
|---|---|---|---|---|
| Скорость | 50-200 KB/s | 200-500 KB/s | 3 KB/s | **500+ KB/s** |
| Пути | 1 | 3-8 | 80 | **200+** |
| Транспорты | 1 | 1 | 4 | **4** |
| Автопоиск DNS | Нет | Нет | Нет | **Да** |
| QUIC | Нет | picoquic (C) | Нет | **quinn (Rust)** |
| Кроссплатформа | Нет | Нет | Win | **Win/Android/iOS** |
| C-зависимости | Go | picoquic | 0 | **0** |
