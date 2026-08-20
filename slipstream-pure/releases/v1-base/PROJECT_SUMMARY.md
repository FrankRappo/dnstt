# Slipstream Pure — Итоговая документация проекта

**Дата:** 2026-03-24
**Статус:** v1.5 — рабочий, стабильный

---

## Что это

Собственный DNS-туннель на чистом Rust. Весь трафик идёт исключительно через DNS-запросы к публичным резолверам (Google, Cloudflare, Quad9 и др.). Прямого IP-соединения между клиентом и сервером нет.

## Что работает

| Функция | Статус |
|---|---|
| HTTP через туннель | **Работает** |
| HTTPS через туннель | **Работает** |
| Multipath (80 путей) | **Работает** |
| 4 транспорта (UDP, TCP, DoH, DoH+ECH) | **Работает** |
| Good-path tracking | **Работает** |
| Adaptive pacing | **Работает** |
| Stream multiplexing | **Работает** |
| Windows .exe | **Собирается** |
| Скорость | **9-11 KB/s** |

## Архитектура

```
Приложение
    ↓
SOCKS5 прокси (sp-client, 127.0.0.1:7000)
    ↓
DNS-запросы через 80 good paths (18 резолверов × 4 транспорта)
    ↓
Публичные DNS-резолверы (Google, Cloudflare, Quad9...)
    ↓
sp-server (порт 53, авторитетный DNS для tunnel.example.com)
    ↓
TCP-подключение к сайту назначения
    ↓
Интернет
```

## Быстрый старт

### Сервер (VPS 203.0.113.130):
```bash
cd /opt && ./sp-server --domain tunnel.example.com --dns-listen 203.0.113.130:53
```

### Клиент (Linux):
```bash
cd /opt && ./sp-client --domain tunnel.example.com --config resolvers.toml --insecure
```

### Клиент (Windows):
```powershell
.\sp-client.exe --domain tunnel.example.com --config resolvers.toml --insecure
```

### Проверка:
```bash
curl -x socks5://127.0.0.1:7000 http://ifconfig.me   # → 203.0.113.130
curl -x socks5://127.0.0.1:7000 https://ifconfig.me  # → 203.0.113.130
```

## Ключевые технические решения

### Что работает хорошо
- **ACK-based downstream delivery** — надёжная доставка данных без потерь
- **Good-path tracking** — система запоминает рабочие пути при старте через Hello echo
- **Sequential polling через 10 parallel paths** — первый ответ побеждает
- **Lazy TCP connect** — сервер подключается к destination только при получении данных
- **Seq dedup** — дедупликация от DNS-ретрансмитов
- **QNAME dedup** — сервер не отдаёт данные повторно на ретрансмит
- **Hash dedup на клиенте** — отсечение дублей downstream
- **DNS response < 512 байт** — безопасно для всех резолверов

### Что пробовали и не работает
| Подход | Почему не работает |
|---|---|
| EDNS0 OPT upstream (1232 байт) | Публичные резолверы стрипают custom EDNS0 options |
| Concurrent pump (15 polls) | Все получают один batch (ACK ограничивает) |
| Streaming delivery | Потеря DNS-ответов 1-5% → TLS handshake ломается |
| DNS-ответы > 512 байт | Резолверы дропают/обрезают |

### Фундаментальное ограничение
ACK-based delivery через один поток: **batch_size / RTT = 500 байт / 50мс = 10 KB/s**

## Файлы проекта

```
/work/vpn/slipstream-pure/
├── PROJECT_SUMMARY.md      — этот файл
├── GUIDE.md                — полное руководство по установке и использованию
├── ROADMAP.md              — план развития v1.5 → v5.0
├── STATUS.md               — технический статус
├── ARCHITECTURE.md         — архитектура
├── Cargo.toml / Cargo.lock
├── config/
│   └── resolvers.toml      — 18 DNS-провайдеров (35+ IP)
├── binaries/
│   ├── sp-client            — Linux x86_64 (4.8 MB)
│   ├── sp-server            — Linux x86_64 (2.8 MB)
│   └── sp-client.exe        — Windows x86_64 (4.5 MB)
└── crates/
    ├── sp-common/           — DNS codec, compression, crypto, protocol
    ├── sp-transport/        — DNS UDP, TCP, DoH, DoH+ECH
    ├── sp-multipath/        — Router, metrics, pacing, good-path tracking
    ├── sp-client/           — SOCKS5 proxy + tunnel client
    └── sp-server/           — DNS listener + TCP forwarding
```

Исходники также в: `/home/hgff/slipstream-pure/`

## Бэкапы

| Файл | Версия | Скорость |
|---|---|---|
| `slipstream-pure-backup-20260324-0011.tar.gz` | v1.0 ранний | — |
| `slipstream-pure-WORKING-20260324-0901.tar.gz` | v1.0 | 3 KB/s |
| `slipstream-pure-FINAL-20260324-1115.tar.gz` | v1.0 финал | 3 KB/s |
| `slipstream-pure-v1-WORKING-HTTP-HTTPS-20260324-1126.tar.gz` | v1.0 HTTP+HTTPS | 3 KB/s |
| `slipstream-pure-v1.5-9KBs-STABLE-20260324-1428.tar.gz` | v1.5 | 9-11 KB/s |
| **`slipstream-pure-v1.5-FINAL-20260324-1516.tar.gz`** | **v1.5 финал** | **9-11 KB/s** |

## Серверы

| Роль | IP | Порт SSH | Логин | Пароль | Файл с данными |
|---|---|---|---|---|---|
| Сервер туннеля | 203.0.113.130 | 22 | root | см. файл | `/work/vpn/dnstt тонель/credentials.example.md` |
| Клиент (тестовый) | 203.0.113.181 | 22 | root | см. файл | `/home/hgff/work/vps.txt` |

## DNS

- **Домен:** example.com (Njalla, активен до 2027-03-21)
- **API Njalla:** см. `/work/vpn/dnstt тонель/credentials.example.md`
- **NS:** tunnel.example.com → ns1.example.com → 203.0.113.130

## 18 DNS-резолверов

| # | Провайдер | IP | DoH | ECH |
|---|---|---|---|---|
| 1 | Cloudflare | 1.1.1.1, 1.0.0.1 | Да | Да |
| 2 | Google | 8.8.8.8, 8.8.4.4 | Да | — |
| 3 | Quad9 | 9.9.9.9, 149.112.112.112 | Да | Да |
| 4 | OpenDNS | 208.67.222.222, 208.67.220.220 | Да | — |
| 5 | AdGuard | 94.140.14.14, 94.140.15.15 | Да | Да |
| 6 | CleanBrowsing | 185.228.168.9, 185.228.169.9 | Да | — |
| 7 | NextDNS | 45.90.28.0, 45.90.30.0 | Да | Да |
| 8 | Mullvad | 194.242.2.2 | Да | Да |
| 9 | DNS.SB | 185.222.222.222, 45.11.45.11 | Да | Да |
| 10 | AliDNS | 223.5.5.5, 223.6.6.6 | Да | — |
| 11 | DNSPod | 119.29.29.29 | Да | — |
| 12 | Comodo | 8.26.56.26, 8.20.247.20 | — | — |
| 13 | Neustar | 64.6.64.6, 64.6.65.6 | — | — |
| 14 | Level3 | 4.2.2.1, 4.2.2.2 | — | — |
| 15 | Yandex | 77.88.8.8, 77.88.8.1 | — | — |
| 16 | Freenom | 80.80.80.80, 80.80.81.81 | — | — |
| 17 | UncensoredDNS | 91.239.100.100 | — | — |
| 18 | Hurricane Electric | 74.82.42.42 | — | — |

## План развития (кратко)

| Версия | Что | Скорость | Статус |
|---|---|---|---|
| v1.0 | Базовый туннель | 3 KB/s | Готово |
| v1.5 | Data pump, ACK delivery | 9-11 KB/s | **Готово (текущая)** |
| v2.0 | N sub-streams (N × 10 KB/s) | 100-150 KB/s | Следующий шаг |
| v2.5 | DNS-сканер (200+ резолверов) | 200-300 KB/s | Запланировано |
| v3.0 | QUIC (quinn) | 300-500 KB/s | Запланировано |
| v4.0 | Мобильные клиенты (Android, iOS) | — | Запланировано |

Подробный roadmap: `ROADMAP.md`

## Оригинальный Slipstream (для справки)

Также установлен оригинальный slipstream-rust-plus:
- Путь: `/work/vpn/dnstt тонель/`
- Работал с 3 multipath путями
- Ограничен picoquic (C-зависимость, макс 3-8 путей)
- Не компилируется под Windows/Android
- Документация: `/work/vpn/dnstt тонель/README.md`
