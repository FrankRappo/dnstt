# Slipstream Pure — Architecture

## Overview

DNS tunnel на чистом Rust. Без C-зависимостей. Кроссплатформенный (Windows, Android, iOS, Linux, macOS).
Совместим с протоколом slipstream-server (QUIC over DNS), но без ограничений picoquic.

## Components

```
┌─────────────────────────────────────────────────────┐
│                    CLIENT                            │
│                                                     │
│  App Traffic → SOCKS5 Proxy (127.0.0.1:7000)       │
│       ↓                                             │
│  QUIC Connection (quinn)                            │
│       ↓                                             │
│  Transport Manager (автовыбор лучшего транспорта)   │
│       ↓                                             │
│  ┌──────────┬──────────┬──────────┬──────────┐     │
│  │ DNS UDP  │ DNS TCP  │   DoH    │ DoH+ECH  │     │
│  │ port 53  │ port 53  │ port 443 │ port 443 │     │
│  └──────────┴──────────┴──────────┴──────────┘     │
│       ↓              ↓            ↓          ↓      │
│  Multipath Router (все резолверы параллельно)       │
│       ↓                                             │
│  Rate Limiter + Adaptive Pacing                     │
│       ↓                                             │
│  DNS Encoder (данные → DNS-запросы к *.t.domain)    │
└─────────────────────────────────────────────────────┘
                    ↓ DNS queries
          ┌─────────────────────┐
          │   DNS Resolvers     │
          │ (Google, CF, etc.)  │
          └─────────────────────┘
                    ↓ forwarded
┌─────────────────────────────────────────────────────┐
│                    SERVER                            │
│                                                     │
│  DNS Listener (port 53, authoritative for t.domain) │
│       ↓                                             │
│  DNS Decoder (DNS-запросы → данные)                 │
│       ↓                                             │
│  QUIC Connection (quinn)                            │
│       ↓                                             │
│  SOCKS5 Proxy (127.0.0.1:1080) → Internet          │
└─────────────────────────────────────────────────────┘
```

## Transport Layer

### 4 транспорта (автовыбор для каждого резолвера)

1. **DNS UDP (порт 53)** — базовый, самый быстрый, ~512 байт на пакет (4096 с EDNS0)
2. **DNS TCP (порт 53)** — fallback, до 65535 байт на пакет
3. **DoH (порт 443)** — HTTPS POST к DoH-эндпоинту, устойчив к блокировке
4. **DoH + ECH (порт 443)** — максимальная маскировка, SNI зашифрован

### Автоматический выбор транспорта

Для каждого резолвера система:
1. Проверяет доступные транспорты (из конфига)
2. Пробует лучший (DoH+ECH → DoH → DNS TCP → DNS UDP)
3. Если текущий заблокирован — автоматически переключается на следующий
4. Периодически пробует вернуться на лучший

## Multipath

- Все резолверы работают параллельно
- Нет лимита на количество путей (динамическое выделение)
- Каждый резолвер — отдельный путь с независимым транспортом
- Connection ID управление полностью в Rust (без picoquic)

## Rate Limiting Protection

- Мониторинг ответов от каждого резолвера
- Метрики: latency, packet loss, SERVFAIL ratio
- Adaptive pacing: автоматическое снижение нагрузки при rate-limit
- Ротация: временное отключение перегруженных резолверов
- Равномерное распределение нагрузки

## DNS Encoding

Данные кодируются в DNS-запросы:
- Upstream (клиент→сервер): данные в subdomain labels `<data>.t.domain.com`
- Downstream (сервер→клиент): данные в DNS TXT/NULL/CNAME ответах
- Base32/Base64 кодирование для DNS-safe символов
- Сжатие (zstd/lz4) перед кодированием

## Crypto

- TLS 1.3 через QUIC (quinn + rustls)
- Самоподписанные сертификаты (как текущий slipstream)
- ECH через rustls (для DoH+ECH транспорта)

## Crate Structure

```
slipstream-pure/
├── Cargo.toml              — workspace
├── config/
│   └── resolvers.toml      — конфиг резолверов (18 провайдеров)
├── crates/
│   ├── sp-common/          — общие типы, DNS encoding/decoding, crypto
│   ├── sp-transport/       — 4 транспорта (DNS UDP, TCP, DoH, DoH+ECH)
│   ├── sp-multipath/       — multipath router, rate limiter, adaptive pacing
│   ├── sp-client/          — клиент (SOCKS5 proxy + QUIC + transports)
│   └── sp-server/          — сервер (DNS listener + QUIC + SOCKS5 forwarder)
└── ARCHITECTURE.md
```

## Key Dependencies (all pure Rust)

- `quinn` — QUIC implementation
- `rustls` — TLS 1.3
- `hickory-dns` — DNS parsing/encoding
- `reqwest` — HTTP client (для DoH)
- `tokio` — async runtime
- `socks5-server` — SOCKS5 proxy
- `toml` / `serde` — config parsing
- `zstd` / `lz4_flex` — compression
- `tracing` — logging

## Cross-compilation Targets

- `x86_64-unknown-linux-gnu` — Linux
- `x86_64-pc-windows-msvc` — Windows (.exe)
- `aarch64-linux-android` — Android
- `aarch64-apple-ios` — iOS
- `aarch64-apple-darwin` — macOS ARM
- `x86_64-apple-darwin` — macOS Intel

## Compatibility

Протокол должен быть совместим с существующим slipstream-server на VPS (203.0.113.130).
Если это невозможно — переписываем и сервер.
