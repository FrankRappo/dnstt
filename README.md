# DNSTT / Slipstream protocol research workspace

> [!IMPORTANT]
> This is a **sanitized public source export**. Production addresses, domains,
> passwords, API tokens, certificates, private keys, reset seeds, nested Git
> metadata, build caches, historical tar archives, and opaque binaries are not
> included. All operational addresses use RFC 5737 examples.

## Current protocol

The newest in-repo protocol implementation is
[`slipstream-pure/dev/v1.8-windowed-pacing`](./slipstream-pure/dev/v1.8-windowed-pacing/).
Its current architecture is the `v3.0-single-session` design: one session across
all DNS resolvers, streaming response processing, dynamic window negotiation,
auto-discovery, and zero-loss multipath behavior in the recorded benchmarks.
See [`SESSION_v3.0.md`](./slipstream-pure/dev/v1.8-windowed-pacing/SESSION_v3.0.md).

The original `Fox-Fig/slipstream-rust-plus` source is retained as a Git submodule
at `slipstream-rust-plus`; clone this repository with `--recurse-submodules`.

## Build

```bash
cd slipstream-pure/dev/v1.8-windowed-pacing
cargo build --release
```

Generated `target/` trees and historical binaries are intentionally omitted.
See [`PUBLIC_EXPORT.md`](./PUBLIC_EXPORT.md) for the complete export policy.

---

## Legacy project notes

# Slipstream DNS Tunnel — Документация

## Архитектура

```
Клиент (SOCKS5 :7000) → DNS-запросы → Резолверы (8.8.8.8 и т.д.) → VPS-сервер (:53) → Dante SOCKS5 (:1080) → Интернет
```

Весь трафик идёт исключительно через DNS (порт 53 UDP).

## Серверы

### VPS-сервер (slipstream-server)
- **IP:** 203.0.113.130
- **Домен:** tunnel.example.com
- **NS-запись:** tunnel.example.com → ns1.example.com → 203.0.113.130
- **Путь бинарника:** /opt/slipstream-rust-plus/target/release/slipstream-server
- **Сертификаты:** /opt/slipstream-rust-plus/cert.pem, key.pem
- **Reset seed:** /opt/slipstream-rust-plus/reset.seed
- **Лог:** /var/log/slipstream.log
- **SOCKS5 прокси:** Dante на 127.0.0.1:1080

#### Запуск сервера:
```bash
cd /opt/slipstream-rust-plus
./target/release/slipstream-server \
  --dns-listen-host 203.0.113.130 \
  -d tunnel.example.com \
  -c cert.pem \
  -k key.pem \
  --reset-seed reset.seed \
  -a 127.0.0.1:1080
```

#### Проверка статуса:
```bash
ps aux | grep slipstream-server
ss -ulnp | grep :53
systemctl status danted
```

### VPS-клиент (slipstream-client) — тестовый
- **IP:** 203.0.113.181
- **Путь бинарника:** /opt/slipstream-rust-plus/target/release/slipstream-client
- **Сертификат:** /opt/slipstream-rust-plus/cert.pem (скопирован с сервера)
- **Скрипт запуска:** /opt/slipstream-rust-plus/start-client.sh
- **Лог:** /var/log/slipstream-client.log
- **Локальный SOCKS5:** 127.0.0.1:7000

#### Запуск клиента:
```bash
cd /opt/slipstream-rust-plus
./target/release/slipstream-client \
  -d tunnel.example.com \
  --cert cert.pem \
  -l 7000 \
  -r 8.8.8.8 -r 8.8.4.4 \
  -r 1.1.1.1 -r 1.0.0.1 \
  -r 9.9.9.9 -r 149.112.112.112 \
  -r 208.67.222.222 -r 208.67.220.220 \
  -r 94.140.14.14 -r 94.140.15.15 \
  -r 185.228.168.9 -r 185.228.169.9 \
  -r 8.26.56.26 -r 8.20.247.20 \
  -r 64.6.64.6 -r 64.6.65.6 \
  -r 4.2.2.1 -r 4.2.2.2 \
  -r 77.88.8.8 -r 77.88.8.1 \
  -r 185.222.222.222 -r 45.11.45.11 \
  -r 194.242.2.2 \
  -r 45.90.28.0 -r 45.90.30.0 \
  -r 223.5.5.5 -r 223.6.6.6 \
  -r 119.29.29.29 \
  -r 80.80.80.80 -r 80.80.81.81 \
  -r 91.239.100.100 \
  -r 74.82.42.42
```

#### Проверка работы:
```bash
# Должен показать IP сервера (203.0.113.130)
curl -x socks5://127.0.0.1:7000 https://ifconfig.me
```

## Результаты тестирования

- **Статус:** РАБОТАЕТ
- **Подключение:** Успешно через 3 резолвера (8.8.8.8, 8.8.4.4, 1.1.1.1)
- **Multipath:** Подключаются максимум 3 пути из 32
- **Причина ограничения:** picoquic хардкод PICOQUIC_NB_PATH_TARGET=8, PICOQUIC_NB_PATH_DEFAULT=2
- **Исходящий IP через туннель:** 203.0.113.130 (IP сервера)
- **Протокол:** QUIC over DNS (порт 53 UDP)

## Текущие ограничения

1. **Multipath лимит:** picoquic ограничивает до 8 путей (реально подключаются 3)
   - Файл: vendor/picoquic/picoquic/picoquic_internal.h
   - Параметры: PICOQUIC_NB_PATH_TARGET, PICOQUIC_NB_PATH_DEFAULT
   - Требуется патч и пересборка для увеличения
2. **Кросс-компиляция:** Windows и Android не собираются из-за C-зависимости picoquic
   - Android: ошибка совместимости picotls с OpenSSL
   - Windows: POSIX-структуры в slipstream-ffi несовместимы с MinGW
3. **Клиенты для мобильных:** Использовать DNSTT.XYZ (поддерживает Slipstream)

## DNS-резолверы (полный список)

| Провайдер | IP | IP2 |
|---|---|---|
| Google | 8.8.8.8 | 8.8.4.4 |
| Cloudflare | 1.1.1.1 | 1.0.0.1 |
| Quad9 | 9.9.9.9 | 149.112.112.112 |
| OpenDNS | 208.67.222.222 | 208.67.220.220 |
| AdGuard | 94.140.14.14 | 94.140.15.15 |
| CleanBrowsing | 185.228.168.9 | 185.228.169.9 |
| Comodo | 8.26.56.26 | 8.20.247.20 |
| Neustar | 64.6.64.6 | 64.6.65.6 |
| Level3 | 4.2.2.1 | 4.2.2.2 |
| Yandex | 77.88.8.8 | 77.88.8.1 |
| DNS.SB | 185.222.222.222 | 45.11.45.11 |
| Mullvad | 194.242.2.2 | — |
| NextDNS | 45.90.28.0 | 45.90.30.0 |
| AliDNS | 223.5.5.5 | 223.6.6.6 |
| DNSPod | 119.29.29.29 | — |
| Freenom | 80.80.80.80 | 80.80.81.81 |
| UncensoredDNS | 91.239.100.100 | — |
| Hurricane Electric | 74.82.42.42 | — |

## DNS-настройка (Njalla)

- **Домен:** example.com (активен до 2027-03-21)
- **API:** Njalla (njal.la/api/1/)
- **Записи:**
  - NS: tunnel.example.com → ns1.example.com (TTL 3600)
  - A: ns1.example.com → 203.0.113.130 (TTL 3600)

## Конфигурация Dante SOCKS5 (на сервере 203.0.113.130)

Файл: `/etc/danted.conf`
```
logoutput: syslog
internal: 127.0.0.1 port = 1080
external: eth0
socksmethod: none
clientmethod: none

client pass {
    from: 127.0.0.1/32 to: 0.0.0.0/0
}
socks pass {
    from: 127.0.0.1/32 to: 0.0.0.0/0
}
```

Управление:
```bash
systemctl restart danted
systemctl status danted
```

## Параметры slipstream-server (все флаги)

| Флаг | Значение по умолчанию | Описание |
|---|---|---|
| --dns-listen-host | :: | IP для прослушивания DNS |
| -l, --dns-listen-port | 53 | Порт DNS |
| -a, --target-address | 127.0.0.1:5201 | Куда проксировать трафик |
| -c, --cert | — | Путь к TLS-сертификату |
| -k, --key | — | Путь к TLS-ключу |
| --reset-seed | — | Путь к файлу seed (32 hex символа) |
| -d, --domain | — | Домен туннеля |
| --max-connections | 256 | Макс. соединений |
| --idle-timeout-seconds | 60 | Таймаут неактивности |
| --fallback | — | Fallback DNS-сервер |

## Параметры slipstream-client (все флаги)

| Флаг | Значение по умолчанию | Описание |
|---|---|---|
| --tcp-listen-host | :: | IP для SOCKS5 |
| -l, --tcp-listen-port | 5201 | Порт SOCKS5 |
| -r, --resolver | — | DNS-резолвер (можно несколько) |
| -d, --domain | — | Домен туннеля |
| --cert | — | Путь к TLS-сертификату сервера |
| -c, --congestion-control | — | bbr или dcubic |
| --authoritative | — | Прямой DNS-сервер (минуя резолверы) |
| -g, --gso | false | Generic Segmentation Offload |
| -t, --keep-alive-interval | 400 | Интервал keep-alive (мс) |

## Сборка из исходников

### Зависимости (apt):
```bash
apt install -y cmake pkg-config libssl-dev build-essential git
```

### Установка Rust:
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source ~/.cargo/env
```

### Клонирование и сборка:
```bash
git clone https://github.com/Fox-Fig/slipstream-rust-plus.git
cd slipstream-rust-plus
git submodule update --init --recursive
cargo build -p slipstream-server -p slipstream-client --release
```

Бинарники: `target/release/slipstream-server`, `target/release/slipstream-client`

### Генерация TLS-сертификатов:
```bash
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
  -keyout key.pem -out cert.pem -days 365 -nodes -subj '/CN=slipstream'
```

### Генерация reset seed:
```bash
openssl rand -hex 16 > reset.seed
```

## Файлы проекта

```
/work/vpn/dnstt тонель/
├── README.md                    — эта документация
├── credentials.example.md               — API-ключ Njalla
├── credentials.example.md                   — данные VPS-сервера
├── binaries/
│   ├── server/
│   │   └── slipstream-server    — рабочий бинарник сервера (Linux x86_64)
│   ├── client/
│   │   ├── slipstream-client    — рабочий бинарник клиента (Linux x86_64)
│   │   └── start-client.sh      — скрипт запуска с 32 резолверами
│   └── certs/
│       ├── cert.pem             — TLS-сертификат (нужен и серверу и клиенту)
│       └── reset.seed           — seed сервера
└── slipstream-rust-plus/        — исходный код (клонирован с GitHub)
```

## Версии ПО

- **Slipstream:** Fox-Fig/slipstream-rust-plus (GitHub, latest main)
- **Сервер ОС:** Ubuntu 22.04 LTS (203.0.113.130)
- **Клиент ОС:** Ubuntu 24.04 LTS (203.0.113.181)
- **Dante SOCKS:** из apt (Ubuntu 22.04)
- **Rust:** latest stable (установлен через rustup)
