# Task: Создать Slipstream Pure — DNS tunnel на чистом Rust

## Контекст

Проект: DNS-туннель для обхода блокировок. Переписываем slipstream-rust-plus (https://github.com/Fox-Fig/slipstream-rust-plus) на чистый Rust без C-зависимостей (picoquic). Причина: picoquic ограничивает multipath до 3 путей, не компилируется под Windows/Android/iOS.

## Что уже есть

- Рабочий slipstream-server на VPS 203.0.113.130 (порт 53, домен tunnel.example.com)
- Рабочий slipstream-client на VPS 203.0.113.181 (тестовый)
- Конфиг резолверов: `/work/vpn/slipstream-pure/config/resolvers.toml` (18 провайдеров, 35+ IP)
- Архитектура: `/work/vpn/slipstream-pure/ARCHITECTURE.md`

## Требования

### Функциональные
1. DNS-туннель: данные кодируются в DNS-запросы к *.tunnel.example.com
2. 4 транспорта: DNS UDP, DNS TCP, DoH, DoH+ECH — автовыбор лучшего
3. Multipath: все 18 резолверов из конфига работают параллельно, без лимита путей
4. Rate-limit protection: adaptive pacing, мониторинг, ротация перегруженных резолверов
5. SOCKS5 прокси на клиенте (127.0.0.1:7000)
6. SOCKS5 прокси на сервере (127.0.0.1:1080) через Dante или встроенный
7. Конфиг из TOML-файла (resolvers.toml)
8. TLS шифрование (самоподписанные сертификаты)

### Нефункциональные
1. Чистый Rust — никаких C-зависимостей
2. Кроссплатформенность: Linux, Windows, Android, iOS, macOS
3. Без ограничений на количество путей
4. Автоматический выбор транспорта и резолверов
5. Сжатие трафика

## Этапы реализации

### Этап 1: Scaffold проекта
- Создать Cargo workspace с crates: sp-common, sp-transport, sp-multipath, sp-client, sp-server
- Настроить зависимости: quinn, rustls, hickory-dns, reqwest, tokio, serde, toml, tracing
- Парсинг resolvers.toml в структуры

### Этап 2: DNS Encoding/Decoding (sp-common)
- Кодирование данных в DNS subdomain labels (base32)
- Декодирование DNS ответов (TXT/NULL записи)
- Сжатие данных (lz4)
- Генерация/валидация TLS сертификатов

### Этап 3: Transport Layer (sp-transport)
- DNS UDP транспорт — отправка/получение через UDP сокет на порт 53
- DNS TCP транспорт — через TCP соединение на порт 53
- DoH транспорт — HTTPS POST к DoH URL
- DoH+ECH транспорт — DoH с Encrypted Client Hello
- Trait `Transport` с единым интерфейсом для всех

### Этап 4: Multipath + Rate Limiting (sp-multipath)
- Router: распределяет пакеты по резолверам
- Метрики на каждый резолвер: latency, loss, throughput
- Adaptive pacing: снижение нагрузки при rate-limit
- Автопереключение транспорта при блокировке
- Динамическое добавление/удаление путей

### Этап 5: Сервер (sp-server)
- DNS listener на порту 53 (авторитетный для tunnel.example.com)
- Декодирование входящих DNS-запросов
- QUIC соединение через quinn
- Forwarding трафика на SOCKS5 (Dante на 127.0.0.1:1080)
- TLS с самоподписанным сертификатом

### Этап 6: Клиент (sp-client)
- SOCKS5 proxy на 127.0.0.1:7000
- Загрузка конфига из resolvers.toml
- Инициализация всех резолверов и транспортов
- QUIC соединение через quinn
- Multipath router для распределения по резолверам
- CLI: --domain, --cert, --listen-port, --config

### Этап 7: Локальная сборка и проверка
- Собрать оба бинарника: `cargo build -p sp-client -p sp-server --release`
- Убедиться что компилируется без ошибок
- Запустить `cargo test` — все тесты должны пройти
- Бинарники должны быть в target/release/sp-client и target/release/sp-server
- Деплой на VPS и интеграционное тестирование — в следующей сессии

### Этап 8: Кроссплатформенная сборка
- Установить cross-compilation targets:
  - `rustup target add x86_64-pc-windows-gnu` (Windows)
  - `rustup target add aarch64-linux-android` (Android)
  - `rustup target add aarch64-apple-ios` (iOS)
  - `rustup target add aarch64-apple-darwin` (macOS ARM)
- Установить линковщики: `apt-get install -y gcc-mingw-w64-x86-64`
- Собрать клиент под все платформы:
  - `cargo build -p sp-client --release --target x86_64-pc-windows-gnu`
  - `cargo build -p sp-client --release` (Linux — дефолт)
- Убедиться что ВСЕ сборки проходят без ошибок
- Сервер собирается только под Linux (он работает на VPS)

## Критически важно

- Код должен компилироваться под Windows (x86_64-pc-windows-gnu) БЕЗ C-зависимостей
- Не использовать `libc` crate напрямую — использовать кроссплатформенные абстракции
- Для сокетов использовать `tokio::net`, не `std::os::unix`
- Для файловых путей использовать `std::path::Path`, не хардкодить `/`
- Тестировать компиляцию под Windows на каждом этапе

## Технические детали

### DNS Encoding формат
- Upstream: `<base32-data>.<session-id>.tunnel.example.com` → тип A/AAAA запрос
- Downstream: DNS ответ с данными в TXT record (base64)
- Max upstream per query: ~200 байт (DNS label limit 63 chars × ~3 labels)
- Max downstream per response: ~4000 байт (с EDNS0)

### QUIC через DNS
- quinn для QUIC стека
- Вместо прямого UDP — QUIC пакеты кодируются в DNS-запросы
- Каждый DNS-запрос = один QUIC пакет (или фрагмент)
- Сервер собирает QUIC пакеты из DNS-запросов и передаёт quinn

### Multipath
- Каждый резолвер = отдельный "virtual path"
- quinn поддерживает multipath нативно
- Нет фиксированных массивов — всё динамическое (Vec, HashMap)
- CID генерируются по требованию без лимита

## Файлы проекта

```
/work/vpn/slipstream-pure/
├── Cargo.toml
├── config/resolvers.toml       ← уже создан
├── ARCHITECTURE.md             ← уже создан
├── crates/
│   ├── sp-common/
│   │   ├── Cargo.toml
│   │   └── src/lib.rs         — DNS encode/decode, compression, crypto
│   ├── sp-transport/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs         — Transport trait
│   │       ├── dns_udp.rs     — DNS UDP transport
│   │       ├── dns_tcp.rs     — DNS TCP transport
│   │       ├── doh.rs         — DoH transport
│   │       └── doh_ech.rs     — DoH+ECH transport
│   ├── sp-multipath/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs         — Multipath router
│   │       ├── metrics.rs     — Per-resolver metrics
│   │       └── pacing.rs      — Adaptive rate control
│   ├── sp-client/
│   │   ├── Cargo.toml
│   │   └── src/main.rs        — Client binary
│   └── sp-server/
│       ├── Cargo.toml
│       └── src/main.rs        — Server binary
```

## Важно

- НЕ использовать C-библиотеки (picoquic, openssl) — только чистый Rust
- НЕ копировать код из slipstream-rust-plus — писать с нуля
- Протокол должен быть свой (не совместим со старым slipstream — это ОК)
- Приоритет: рабочий туннель > все фичи. Сначала DNS UDP multipath, потом DoH/ECH
- Конфиг резолверов уже создан в config/resolvers.toml — использовать его
