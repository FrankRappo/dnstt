# Fix Task 2: Stream lifecycle bug — "stream not found, dropping data"

## Проект
`/home/hgff/slipstream-pure/` — DNS tunnel на чистом Rust.

## Текущее состояние

Сервер и клиент запускаются, 95 путей инициализируются, DNS-запросы доходят до сервера, сервер получает Connect и подключается к destination. НО данные не проходят:

```
[INFO] session 391c87e10e4035db stream 1 connecting to 34.160.111.145:443
[INFO] stream 1 connected to 34.160.111.145:443
[DEBUG] session 391c87e10e4035db stream 1 not found, dropping data
```

Stream создаётся (insert в streams HashMap), TCP-соединение устанавливается, но через 400мс при получении Data пакета stream уже не найден.

## Вероятные причины

1. **stream_forward_task завершается слишком быстро** — TCP-соединение к destination (HTTPS) ждёт данные, но upstream_rx пуст. Если upstream_rx закрыт (sender dropped), task завершается и удаляет stream из HashMap (строка 330-332 в server main.rs).

2. **Sender dropped** — StreamHandle.tx может быть dropped если Session пересоздаётся или если Mutex lock конфликтует.

3. **TLS handshake timeout** — ifconfig.me ожидает TLS ClientHello, но данные приходят маленькими кусками через DNS (80 байт за раз). TLS ClientHello ~300 байт, нужно 4+ DNS-запроса. Сервер должен буферизировать и отправлять в TCP только когда все фрагменты собраны, ИЛИ отправлять каждый кусок по мере получения (TCP сам соберёт).

## Что нужно исправить

### В sp-server/src/main.rs:

1. **Добавить логирование** — при удалении stream логировать причину (task завершился, rx закрыт, TCP ошибка)

2. **stream_forward_task НЕ должен завершаться** пока не получит явный Disconnect или TCP-ошибку. Если upstream_rx пуст — ждать, не завершаться.

3. **Проверить что StreamHandle.tx не дропается преждевременно** — убедиться что Arc<Session> жив во время всей обработки Connect.

4. **Буферизация upstream данных** — каждый Data пакет (80 байт) должен немедленно отправляться в TCP. TCP сам соберёт для TLS handshake.

### В sp-client/src/main.rs:

5. **Проверить что Connect отправляется ДО Data** — клиент должен дождаться подтверждения Connect (или хотя бы задержку) перед отправкой данных. Иначе Data может прийти раньше Connect.

6. **Фрагментация TLS handshake** — клиент читает до 80 байт, но TLS ClientHello ~300 байт. Клиент должен:
   - Читать весь ClientHello (до chunk_size байт за раз)
   - Отправлять каждый chunk как отдельный Data пакет
   - Все chunks должны дойти и собраться на сервере в правильном порядке

## Как тестировать

### На сервере (203.0.113.130):
Доступ: `sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.130`
```bash
killall sp-server
cd /opt && ./sp-server --domain tunnel.example.com --dns-listen 203.0.113.130:53
```

### На клиенте (203.0.113.181):
Доступ: `sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.181`
```bash
killall sp-client
cd /opt && ./sp-client --domain tunnel.example.com --config resolvers.toml --insecure
```

### Тест:
```bash
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.181 "curl -x socks5://127.0.0.1:7000 -s --max-time 30 https://ifconfig.me"
```
Должен вернуть: `203.0.113.130`

### Тест попроще (HTTP без TLS):
```bash
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.181 "curl -x socks5://127.0.0.1:7000 -s --max-time 30 http://ifconfig.me"
```

## Порядок работы

1. Прочитай текущий код сервера и клиента
2. Исправь баги
3. Собери: `cd /home/hgff/slipstream-pure && cargo build -p sp-client -p sp-server --release`
4. Загрузи на серверы:
   - `sshpass -p '<REDACTED_PASSWORD>' scp target/release/sp-server root@203.0.113.130:/opt/sp-server`
   - `sshpass -p '<REDACTED_PASSWORD>' scp target/release/sp-client root@203.0.113.181:/opt/sp-client`
5. Перезапусти оба (убей старые, запусти новые)
6. Протестируй curl через SOCKS5
7. Если не работает — читай логи обоих, фикси, повторяй

## Критерий успеха

`curl -x socks5://127.0.0.1:7000 https://ifconfig.me` с клиентского VPS возвращает `203.0.113.130`

## Важно

- НЕ менять sp-transport и sp-multipath
- НЕ менять resolvers.toml
- Собирать ТОЛЬКО release: `cargo build --release`
- После успешного теста — собрать под Windows: `cargo build -p sp-client --release --target x86_64-pc-windows-gnu`
