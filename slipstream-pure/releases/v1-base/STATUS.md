# Slipstream Pure — Статус проекта

**Дата:** 2026-03-24

## Что сделано

### 1. Оригинальный Slipstream (slipstream-rust-plus)
- Установлен и собран на VPS-сервере (203.0.113.130) и VPS-клиенте (203.0.113.181)
- **Работает** — туннель через DNS подтверждён: `curl -x socks5://127.0.0.1:7000 https://ifconfig.me` → `203.0.113.130`
- **Ограничение:** максимум 3 multipath пути из-за хардкодов в C-библиотеке picoquic
- **Ограничение:** не компилируется под Windows/Android/iOS из-за C-зависимостей
- Бинарники сохранены в `/work/vpn/dnstt тонель/binaries/`

### 2. Slipstream Pure (новый проект на чистом Rust)
- **Написан с нуля** — 5 crates, ~1500 строк Rust-кода
- **0 C-зависимостей** — полностью чистый Rust
- **Компилируется** под Linux и Windows (.exe)
- **95 активных путей** (vs 3 у оригинала) — 18 провайдеров × до 4 транспортов
- **4 транспорта:** DNS UDP, DNS TCP, DoH, DoH+ECH
- **26 тестов** проходят
- **Баг:** данные пока не проходят end-to-end (stream lifecycle issue — в процессе фикса)

### 3. DNS-настройка
- **Домен:** example.com (Njalla, активен до 2027-03-21)
- **NS:** tunnel.example.com → ns1.example.com → 203.0.113.130
- **Работает** — авторитетные серверы отдают правильные записи

### 4. VPS
- **Сервер:** 203.0.113.130 (Ubuntu 22.04, 2GB RAM)
- **Клиент (тестовый):** 203.0.113.181 (Ubuntu 24.04)
- Dante SOCKS5 установлен на сервере (127.0.0.1:1080)

## Архитектура Slipstream Pure

```
App → SOCKS5 (sp-client :7000) → DNS queries → Resolvers → sp-server (:53) → TCP direct → Internet
```

### Crates:
| Crate | Описание | Статус |
|---|---|---|
| sp-common | DNS codec, compression, crypto, protocol | Готов |
| sp-transport | DNS UDP, TCP, DoH, DoH+ECH | Готов |
| sp-multipath | Router, metrics, adaptive pacing | Готов |
| sp-client | SOCKS5 proxy + tunnel client | Работает, stream mux добавлен |
| sp-server | DNS listener + direct TCP forwarding | Работает, stream lifecycle баг |

### Бинарники:
| Файл | Платформа | Размер |
|---|---|---|
| sp-client | Linux x86_64 | 4.6 MB |
| sp-server | Linux x86_64 | 2.8 MB |
| sp-client.exe | Windows x86_64 | 4.4 MB |

## 18 DNS-резолверов

| # | Провайдер | IP | DoH | ECH |
|---|---|---|---|---|
| 1 | Cloudflare | 1.1.1.1, 1.0.0.1 | Да | Да |
| 2 | Google | 8.8.8.8, 8.8.4.4 | Да | Нет |
| 3 | Quad9 | 9.9.9.9, 149.112.112.112 | Да | Да |
| 4 | OpenDNS | 208.67.222.222, 208.67.220.220 | Да | Нет |
| 5 | AdGuard | 94.140.14.14, 94.140.15.15 | Да | Да |
| 6 | CleanBrowsing | 185.228.168.9, 185.228.169.9 | Да | Нет |
| 7 | NextDNS | 45.90.28.0, 45.90.30.0 | Да | Да |
| 8 | Mullvad | 194.242.2.2 | Да | Да |
| 9 | DNS.SB | 185.222.222.222, 45.11.45.11 | Да | Да |
| 10 | AliDNS | 223.5.5.5, 223.6.6.6 | Да | Нет |
| 11 | DNSPod | 119.29.29.29 | Да | Нет |
| 12 | Comodo | 8.26.56.26, 8.20.247.20 | Нет | — |
| 13 | Neustar | 64.6.64.6, 64.6.65.6 | Нет | — |
| 14 | Level3 | 4.2.2.1, 4.2.2.2 | Нет | — |
| 15 | Yandex | 77.88.8.8, 77.88.8.1 | Нет | — |
| 16 | Freenom | 80.80.80.80, 80.80.81.81 | Нет | — |
| 17 | UncensoredDNS | 91.239.100.100 | Нет | — |
| 18 | Hurricane Electric | 74.82.42.42 | Нет | — |

## Что осталось

### v1.0 — ВЫПОЛНЕНО 2026-03-24
- [x] HTTP работает
- [x] HTTPS работает
- [x] 80 good paths
- [x] Windows .exe
- [x] Скорость: 3 KB/s

### v1.5 — ВЫПОЛНЕНО 2026-03-24
- [x] Data pump (separate poll task, fire-and-forget upstream)
- [x] Sequential polling через send_parallel_good (10 paths)
- [x] ACK-based downstream delivery
- [x] Hash dedup для дубликатов
- [x] EDNS0 upstream код готов (fallback на QNAME — резолверы стрипают EDNS0)
- [x] HTTP: работает стабильно
- [x] HTTPS: работает стабильно
- [x] Скорость: **9-11 KB/s** (×3.5 от v1.0)
- [x] Good paths: 69-82
- [x] Бэкап: slipstream-pure-v1.5-9KBs-STABLE-20260324-1428.tar.gz

### Следующие шаги
- [ ] **v2.0: N sub-streams** — N независимых ACK-потоков через разные резолверы (N × 10 KB/s = 100+ KB/s)
- [ ] **v2.5: DNS-сканер** — автопоиск 200+ резолверов (×3 скорость)
- [ ] **v3.0: QUIC (quinn)** — замена самописного протокола (убрать потери, +50%)
- [ ] Тест Windows клиента (.exe)
- [ ] Деплой на Android
- [ ] Systemd сервисы для автозапуска
- [ ] iOS клиент

## Файлы

```
/work/vpn/
├── dnstt тонель/                          — оригинальный slipstream
│   ├── binaries/                          — рабочие бинарники оригинала
│   ├── README.md                          — документация оригинала
│   └── slipstream-rust-plus/              — исходники оригинала
├── slipstream-pure/                       — НОВЫЙ проект (чистый Rust)
│   ├── ARCHITECTURE.md                    — архитектура
│   ├── STATUS.md                          — этот файл
│   ├── config/resolvers.toml              — 18 резолверов
│   ├── crates/                            — исходный код
│   ├── binaries/                          — скомпилированные бинарники
│   └── task.md, fix_task.md, fix2_task.md — задания для агента
├── slipstream-pure-backup-*.tar.gz        — бэкапы
```

## Ключевые достижения

1. **HTTP и HTTPS работают** через DNS-туннель — проверено 2026-03-24
2. **95 путей** вместо 3 — убрали ограничение picoquic
3. **4 транспорта** — DNS UDP, TCP, DoH, DoH+ECH (почти неблокируемый)
4. **0 C-зависимостей** — компилируется под все платформы
5. **Windows .exe** (4.4 MB) — собирается из коробки
6. **Good-path tracking** — система запоминает рабочие пути и использует их для downstream
7. **Adaptive rate limiting** — автоотключение перегруженных резолверов
8. **Smart downstream tracking** — не деактивирует пути при нормальных пустых ответах
9. **Stream multiplexing** — несколько соединений через один туннель
10. **Lazy TCP connect** — сервер подключается к destination только при получении данных
11. **Seq dedup** — дедупликация данных от DNS-ретрансмитов
12. **Конфиг из TOML** — добавить резолвер = добавить строку

## Технические решения (fix history)

### Fix 1: Stream lifecycle
- Lazy TCP connect (ждём данные, не подключаемся на Connect)
- TCP_NODELAY для мелких чанков
- Seq-номера и дедупликация upstream

### Fix 2: Downstream delivery
- QNAME dedup на сервере
- Маленькие downstream пакеты (240 байт) чтобы влезали в DNS UDP
- Inline polling вместо отдельной задачи

### Fix 3: HTTPS / Good-path tracking
- Good-paths — запоминаем пути которые доставили данные
- Параллельный poll через good paths
- Time-based retry cache на сервере (400мс)
- Hash dedup на клиенте
- Adaptive poll interval: 20ms для TLS, 50ms для HTTP, 100ms idle
