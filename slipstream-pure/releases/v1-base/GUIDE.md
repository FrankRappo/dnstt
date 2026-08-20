# Slipstream Pure — Полное руководство

## Что это

DNS-туннель на чистом Rust. Весь трафик идёт исключительно через DNS-запросы. Никаких прямых IP-соединений между клиентом и сервером.

Трафик маскируется под обычные DNS-запросы к публичным резолверам (Google, Cloudflare и т.д.). Заблокировать можно только отключив DNS целиком.

## Как работает

```
Приложение → SOCKS5 (sp-client :7000)
  → DNS-запросы через 80 резолверов параллельно
    → Публичные DNS-резолверы (Google, Cloudflare, Quad9...)
      → sp-server (порт 53, авторитетный DNS для tunnel.example.com)
        → TCP-подключение к сайту назначения
          → Интернет
```

## Требования

### Сервер (VPS)
- Linux (Ubuntu 20.04+)
- Публичный IP
- Открытый порт 53 (UDP + TCP)
- Домен с NS-записью указывающей на этот IP

### Клиент
- Linux, Windows, или Android
- Доступ к DNS (порт 53 или DoH через HTTPS)

### DNS-записи (уже настроены для example.com)
```
NS  tunnel.example.com → ns1.example.com
A   ns1.example.com → 203.0.113.130
```

## Установка с нуля

### 1. DNS-настройка

У регистратора домена создать записи:
- A-запись: `ns1.YOURDOMAIN` → `IP_ВАШЕГО_СЕРВЕРА`
- NS-запись: `t.YOURDOMAIN` → `ns1.YOURDOMAIN`

### 2. Сервер

Скопировать `sp-server` на VPS:
```bash
scp sp-server root@YOUR_SERVER_IP:/opt/sp-server
chmod +x /opt/sp-server
```

Если порт 53 занят systemd-resolved:
```bash
# Проверить:
ss -ulnp | grep :53

# Если занят 127.0.0.53:53 — это нормально, не мешает
# Если занят 0.0.0.0:53 — нужно остановить:
systemctl stop systemd-resolved
```

Запуск:
```bash
cd /opt
./sp-server --domain t.YOURDOMAIN --dns-listen YOUR_SERVER_IP:53
```

Сервер автоматически сгенерирует TLS-сертификат (`server.crt`, `server.key`).

Запуск в фоне:
```bash
nohup ./sp-server --domain t.YOURDOMAIN --dns-listen YOUR_SERVER_IP:53 > /var/log/sp-server.log 2>&1 &
```

### 3. Клиент (Linux)

Скопировать `sp-client` и `resolvers.toml`:
```bash
scp sp-client resolvers.toml root@CLIENT_IP:/opt/
chmod +x /opt/sp-client
```

Скопировать сертификат с сервера:
```bash
scp root@YOUR_SERVER_IP:/opt/server.crt /opt/server.crt
```

Запуск:
```bash
cd /opt
./sp-client --domain t.YOURDOMAIN --config resolvers.toml --cert server.crt
```

Или без проверки сертификата:
```bash
./sp-client --domain t.YOURDOMAIN --config resolvers.toml --insecure
```

Запуск в фоне:
```bash
nohup ./sp-client --domain t.YOURDOMAIN --config resolvers.toml --insecure > /var/log/sp-client.log 2>&1 &
```

### 4. Клиент (Windows)

1. Скопировать `sp-client.exe` и `resolvers.toml` в одну папку
2. Открыть PowerShell в этой папке
3. Запустить:
```powershell
.\sp-client.exe --domain t.YOURDOMAIN --config resolvers.toml --insecure
```
4. SOCKS5 прокси будет на `127.0.0.1:7000`

### 5. Использование

Любое приложение с поддержкой SOCKS5-прокси:

**curl:**
```bash
curl -x socks5://127.0.0.1:7000 https://ifconfig.me
```

**Браузер (Firefox):**
- Настройки → Сеть → Прокси → Ручная настройка
- SOCKS Host: `127.0.0.1`, Port: `7000`, SOCKS v5

**Telegram:**
- Настройки → Данные и хранилище → Прокси → SOCKS5
- Сервер: `127.0.0.1`, Порт: `7000`

## Параметры запуска

### sp-server
```
--domain <DOMAIN>          Домен туннеля [default: tunnel.example.com]
--dns-listen <ADDR:PORT>   Адрес для DNS [default: 0.0.0.0:53]
--cert <PATH>              TLS сертификат [default: server.crt]
--key <PATH>               TLS ключ [default: server.key]
```

### sp-client
```
--domain <DOMAIN>          Домен туннеля [default: tunnel.example.com]
--listen <ADDR:PORT>       SOCKS5 адрес [default: 127.0.0.1:7000]
--config <PATH>            Путь к resolvers.toml [default: config/resolvers.toml]
--cert <PATH>              TLS сертификат сервера
--insecure                 Не проверять сертификат
--max-connections <N>      Макс. соединений [default: 64]
```

## Конфигурация резолверов (resolvers.toml)

```toml
[[resolver]]
name = "Cloudflare"
ip = ["1.1.1.1", "1.0.0.1"]
doh = "https://cloudflare-dns.com/dns-query"
ech = true

[[resolver]]
name = "Google"
ip = ["8.8.8.8", "8.8.4.4"]
doh = "https://dns.google/dns-query"

[[resolver]]
name = "Level3"
ip = ["4.2.2.1", "4.2.2.2"]
```

Поля:
- `name` — название (для логов)
- `ip` — обязательно, IP-адреса резолвера
- `doh` — опционально, URL для DNS-over-HTTPS
- `ech` — опционально, поддержка Encrypted Client Hello

Добавить резолвер = добавить блок `[[resolver]]`. Перезапустить клиент.

## 4 транспорта

Для каждого резолвера система автоматически использует лучший доступный транспорт:

| Транспорт | Порт | Когда используется |
|---|---|---|
| DNS UDP | 53 | Базовый, всегда доступен |
| DNS TCP | 53 | Для больших пакетов |
| DoH | 443 | Если указан `doh` в конфиге |
| DoH + ECH | 443 | Если указан `doh` + `ech = true` |

## 18 резолверов (текущий конфиг)

| Провайдер | IP | DoH | ECH |
|---|---|---|---|
| Cloudflare | 1.1.1.1, 1.0.0.1 | Да | Да |
| Google | 8.8.8.8, 8.8.4.4 | Да | Нет |
| Quad9 | 9.9.9.9, 149.112.112.112 | Да | Да |
| OpenDNS | 208.67.222.222, 208.67.220.220 | Да | Нет |
| AdGuard | 94.140.14.14, 94.140.15.15 | Да | Да |
| CleanBrowsing | 185.228.168.9, 185.228.169.9 | Да | Нет |
| NextDNS | 45.90.28.0, 45.90.30.0 | Да | Да |
| Mullvad | 194.242.2.2 | Да | Да |
| DNS.SB | 185.222.222.222, 45.11.45.11 | Да | Да |
| AliDNS | 223.5.5.5, 223.6.6.6 | Да | Нет |
| DNSPod | 119.29.29.29 | Да | Нет |
| Comodo | 8.26.56.26, 8.20.247.20 | Нет | — |
| Neustar | 64.6.64.6, 64.6.65.6 | Нет | — |
| Level3 | 4.2.2.1, 4.2.2.2 | Нет | — |
| Yandex | 77.88.8.8, 77.88.8.1 | Нет | — |
| Freenom | 80.80.80.80, 80.80.81.81 | Нет | — |
| UncensoredDNS | 91.239.100.100 | Нет | — |
| Hurricane Electric | 74.82.42.42 | Нет | — |

## Автоматические механизмы

- **Good-path discovery** — при старте система проверяет все 95 путей, находит ~80 рабочих
- **Adaptive pacing** — если резолвер перегружен, нагрузка на него снижается
- **Деактивация** — нерабочие пути отключаются автоматически
- **Реактивация** — каждые 60 секунд система проверяет отключённые пути
- **Smart downstream tracking** — пути оцениваются по реальной доставке данных
- **Parallel polling** — downstream данные запрашиваются через несколько путей одновременно

## Текущие характеристики (v1.5)

| Параметр | Значение |
|---|---|
| HTTP | Работает стабильно |
| HTTPS | Работает стабильно |
| Download speed | **9-11 KB/s** |
| Good paths | 69-82 из 95 |
| Upstream chunk | 80 байт (QNAME base32) |
| Downstream chunk | 500 байт (ACK-based) |
| DNS response size | ~420 байт (< 512, безопасно для всех резолверов) |
| Poll mechanism | Sequential, 10 parallel good paths |
| C-зависимости | 0 |
| sp-client | 4.8 MB (Linux), 4.5 MB (Windows) |
| sp-server | 2.8 MB |

### Фундаментальное ограничение текущей версии
ACK-based delivery: 500 байт / 50мс RTT = ~10 KB/s на один поток.
Следующий шаг: N независимых sub-streams → N × 10 KB/s.

## Логи и отладка

```bash
# Лог сервера:
tail -f /var/log/sp-server.log

# Лог клиента:
tail -f /var/log/sp-client.log

# Verbose логи:
RUST_LOG=debug ./sp-server ...
RUST_LOG=debug ./sp-client ...

# Проверка что сервер слушает:
ss -ulnp | grep :53

# Проверка DNS-делегации:
dig NS tunnel.example.com @1-you.njalla.no.
dig A test.tunnel.example.com @203.0.113.130

# Проверка туннеля:
curl -x socks5://127.0.0.1:7000 http://ifconfig.me
curl -x socks5://127.0.0.1:7000 https://ifconfig.me
```

## Сборка из исходников

```bash
# Зависимости:
apt install -y build-essential pkg-config libssl-dev gcc-mingw-w64-x86-64

# Rust:
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source ~/.cargo/env
rustup target add x86_64-pc-windows-gnu

# Сборка:
cd /home/hgff/slipstream-pure
cargo build -p sp-client -p sp-server --release

# Windows:
cargo build -p sp-client --release --target x86_64-pc-windows-gnu

# Бинарники:
# Linux:   target/release/sp-client, target/release/sp-server
# Windows: target/x86_64-pc-windows-gnu/release/sp-client.exe
```

## Файловая структура

```
/work/vpn/slipstream-pure/
├── GUIDE.md                — это руководство
├── STATUS.md               — статус проекта
├── ARCHITECTURE.md         — архитектура
├── Cargo.toml              — workspace
├── config/
│   └── resolvers.toml      — 18 провайдеров
├── binaries/
│   ├── sp-client            — Linux (4.8 MB)
│   ├── sp-server            — Linux (2.9 MB)
│   └── sp-client.exe        — Windows (4.5 MB)
└── crates/
    ├── sp-common/           — DNS codec, compression, crypto, protocol
    ├── sp-transport/        — 4 транспорта (DNS UDP, TCP, DoH, DoH+ECH)
    ├── sp-multipath/        — multipath router, metrics, pacing, good-path tracking
    ├── sp-client/           — клиент
    └── sp-server/           — сервер

/work/vpn/
├── slipstream-pure-v1-WORKING-HTTP-HTTPS-*.tar.gz — бэкап рабочей версии
├── dnstt тонель/            — оригинальный slipstream (для справки)
```

## Текущие серверы

| Роль | IP | Пароль | Путь |
|---|---|---|---|
| Сервер туннеля | 203.0.113.130:22 | см. /work/vpn/dnstt тонель/credentials.example.md | /opt/sp-server |
| Клиент (тестовый) | 203.0.113.181:22 | см. /home/hgff/work/vps.txt | /opt/sp-client |

## DNS (Njalla)

- Домен: example.com (до 2027-03-21)
- API: см. /work/vpn/dnstt тонель/credentials.example.md
- Записи: tunnel.example.com NS → ns1.example.com A → 203.0.113.130
