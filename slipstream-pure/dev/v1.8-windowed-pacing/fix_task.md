# Fix Task: Slipstream Pure — сервер не проксирует трафик

## Проект
`/home/hgff/slipstream-pure/` — DNS tunnel на чистом Rust.

## Проблема

Сервер получает данные от клиента через DNS, но не проксирует их в интернет. Клиент отправляет SOCKS5 CONNECT запрос (например к 34.160.111.145:443), но сервер:
1. Создаёт одно TCP-соединение к Dante SOCKS5 (127.0.0.1:1080) на всю сессию
2. Отправляет сырые данные без SOCKS5 handshake
3. Не парсит адрес назначения из клиентских данных

## Что нужно сделать

### Вариант: Сервер сам делает исходящие подключения (без Dante)

Клиент отправляет SOCKS5 CONNECT от приложения. Сервер должен:

1. **Получить адрес назначения** — клиент уже парсит SOCKS5 от приложения и знает куда подключаться. Нужно передать адрес назначения в tunnel protocol.
2. **Добавить в протокол** (sp-common/src/protocol.rs) новый тип пакета `Connect` с полями `host: String, port: u16`
3. **Клиент** (sp-client/src/main.rs) — при получении SOCKS5 CONNECT от приложения, отправить пакет `Connect(host, port)` серверу через DNS-туннель, затем пересылать данные
4. **Сервер** (sp-server/src/main.rs) — при получении `Connect` пакета:
   - Подключиться напрямую к `host:port` через `TcpStream::connect()`
   - Пересылать данные между DNS-туннелем и TCP-соединением
   - Убрать зависимость от Dante SOCKS5

### Также исправить

5. **DNS encode overflow** — клиент читает до 80 байт за раз, но после добавления protocol header + compression + base32 encoding иногда всё равно > 253 символов. Уменьшить chunk_size или правильно рассчитать.

6. **Сервер создаёт новую сессию на каждый DNS-запрос** — в логе видно "new session: 5b5ed3ac2aba6b31" повторяется многократно. Сессия должна создаваться ОДИН РАЗ, последующие запросы с тем же session_id должны переиспользовать существующую.

7. **Stream multiplexing** — каждое SOCKS5 подключение должно иметь свой stream_id внутри сессии. Поле `stream_id` уже есть в TunnelPacket. Использовать его чтобы мультиплексировать несколько TCP-соединений через один DNS-туннель.

## Архитектура после фикса

```
Приложение → SOCKS5 → sp-client
  sp-client: парсит CONNECT host:port
  sp-client: отправляет Connect(stream_id, host, port) через DNS
  sp-client: отправляет Data(stream_id, payload) через DNS

sp-server: получает Connect(stream_id, host, port)
  sp-server: TcpStream::connect(host:port)
  sp-server: привязывает stream_id к TCP-соединению
sp-server: получает Data(stream_id, payload)
  sp-server: пересылает payload в TCP-соединение для stream_id
sp-server: читает ответ из TCP
  sp-server: помещает в downstream_queue как Data(stream_id, response)

sp-client: получает Data(stream_id, response) из DNS-ответа
  sp-client: пересылает response в SOCKS5 соединение для stream_id
```

## Файлы для изменения

1. `crates/sp-common/src/protocol.rs` — добавить PacketType::Connect с host/port
2. `crates/sp-server/src/main.rs` — переписать: прямые TCP-подключения, переиспользование сессий, stream multiplexing
3. `crates/sp-client/src/main.rs` — отправлять Connect пакет с адресом, использовать stream_id, исправить chunk_size

## Тестирование

После фикса:
```bash
# На сервере (203.0.113.130):
./sp-server --domain tunnel.example.com --dns-listen 203.0.113.130:53

# На клиенте (203.0.113.181):
./sp-client --domain tunnel.example.com --config resolvers.toml --insecure

# Тест:
curl -x socks5://127.0.0.1:7000 https://ifconfig.me
# Должен вернуть: 203.0.113.130
```

## Важно

- Dante SOCKS5 больше не нужен — сервер подключается напрямую
- Сохранить 95 путей (multipath работает)
- Сохранить 4 транспорта (DNS UDP, TCP, DoH, DoH+ECH)
- НЕ менять sp-transport и sp-multipath — они работают
- Собрать и проверить что `cargo build --release` и `cargo test` проходят
- Собрать под Windows: `cargo build -p sp-client --release --target x86_64-pc-windows-gnu`
