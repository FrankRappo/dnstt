# Задача: Connection-level retry для HTTPS

## Проблема

HTTPS через DNS-туннель работает только ~50-70%. Причина: TLS ServerHello (~3252 байт = ~14 DNS-ответов) часто не доходит полностью из-за потерь DNS-пакетов. Если хоть один из 14 ответов потерян — TLS handshake не завершится и соединение зависнет.

## Решение

Добавить connection-level retry в клиент (`crates/sp-client/src/main.rs`). Логика:

1. **Буферизировать upstream данные** (первые данные от приложения — ClientHello, ~300-500 байт, максимум 2KB)
2. **При downstream timeout (5 сек без данных):**
   - Отправить Disconnect для текущего stream_id
   - Создать новый stream_id
   - Перерегистрировать stream
   - Отправить Connect заново на тот же host:port
   - Переотправить буферизированные upstream данные
   - Перезапустить poll pump
3. **Максимум 6 попыток** × 5 сек = 30 секунд
4. **Приложение ничего не знает** — просто ждёт чуть дольше

## Целевой результат

HTTPS ~99.8% (вместо 50-70%) при тех же условиях.

## Рабочая директория

`/work/vpn/slipstream-pure-dev/`

## Архитектура (НЕ менять!)

- `crates/sp-client/src/main.rs` — SOCKS5 клиент + туннель (ЕДИНСТВЕННЫЙ файл который нужно менять)
- `crates/sp-server/src/main.rs` — DNS сервер (НЕ трогать)
- `crates/sp-common/src/protocol.rs` — TunnelPacket протокол (НЕ трогать)
- `crates/sp-multipath/src/lib.rs` — MultipathRouter (НЕ трогать)

Серверная сторона уже корректно обрабатывает повторные Connect — при получении Connect для нового stream_id сервер создаёт новое TCP-соединение к целевому хосту. Старый stream автоматически очищается.

## Текущий flow (без retry)

В функции `handle_socks5_connection` (строка ~363):
1. SOCKS5 handshake
2. `send_connect(stream_id, host, port)` — с retry до 6 раз (уже есть)
3. SOCKS5 reply (success)
4. Запуск 3 tokio tasks:
   - **pump_handle** (строка ~476) — poll pump: шлёт Poll+ACK через `send_parallel_good(10)`, dispatch downstream
   - **upstream_handle** (строка ~537) — читает из SOCKS5, шлёт Data через DNS
   - **write_handle** (строка ~590) — пишет downstream данные в SOCKS5 с 5-сек timeout на первые данные
5. Ожидание завершения: `upstream_handle.await`, `pump_handle.abort()`, `write_handle.await`

## Что нужно изменить

Переписать `handle_socks5_connection` добавив retry loop вокруг шагов 2-4:

### Новая логика (псевдокод):

```
fn handle_socks5_connection(stream, peer, client):
    // SOCKS5 handshake (как сейчас, без изменений)
    ...parse dest_addr, dest_port...

    // SOCKS5 reply (как сейчас — ДО retry loop)
    stream.write(success_reply)

    let (read_half, write_half) = stream.split()

    // Буфер для upstream данных (для retry)
    let upstream_buffer: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()))
    let buffer_complete: Arc<AtomicBool> = Arc::new(AtomicBool::new(false))

    // Upstream reader — запускаем ОДИН раз, он заполняет буфер И шлёт данные
    // Первые 2KB — буферизируются (для retry)
    // Остальные — шлём как обычно
    // Этот task живёт всё время, не перезапускается при retry

    const MAX_BUFFER: usize = 2048;
    const MAX_RETRIES: u8 = 6;
    const DOWNSTREAM_TIMEOUT: Duration = Duration::from_secs(5);

    for attempt in 1..=MAX_RETRIES {
        let stream_id = client.next_stream_id()
        let downstream_rx = client.register_stream(stream_id)

        // Connect
        client.send_connect(stream_id, &dest_addr, dest_port)

        // Если retry — переотправить буферизированные данные
        if attempt > 1 {
            let buffered = upstream_buffer.lock().clone()
            for chunk in buffered.chunks(chunk_size) {
                send_data(stream_id, chunk)  // fire-and-forget через good paths
            }
        }

        // Запустить poll pump для этого stream_id
        let pump = spawn_poll_pump(stream_id, ...)

        // Запустить upstream sender (или продолжить существующий)
        // При attempt == 1: запустить reader + sender
        // При attempt > 1: только sender из буфера (reader уже работает)
        let upstream = spawn_upstream_sender(stream_id, read_half, upstream_buffer, ...)

        // Ждать downstream с timeout
        match timeout(DOWNSTREAM_TIMEOUT, downstream_rx.recv()) {
            Ok(Some(data)) if !data.is_empty() => {
                // Есть данные! Retry успешен.
                write_half.write(data)
                // Переход к обычному write loop
                break  // выходим из retry loop
            }
            _ => {
                // Timeout — retry
                pump.abort()
                client.send_disconnect(stream_id)
                client.unregister_stream(stream_id)
                info!("attempt {}/{} failed, retrying...", attempt, MAX_RETRIES)
                continue
            }
        }
    }

    // Обычный write loop (после успешного retry)
    // + ожидание завершения upstream + cleanup
```

### Ключевые нюансы реализации:

1. **Upstream буферизация**: Нужно собирать первые данные от приложения в буфер (до 2KB). Это ClientHello для HTTPS. При retry — переотправить этот буфер через НОВЫЙ stream_id. Данные сверх 2KB не буферизируются (для экономии памяти).

2. **Poll pump**: Перезапускается при каждом retry с новым stream_id. Используй тот же паттерн что сейчас (send_parallel_good + ACK + batch_id).

3. **Upstream sender**: При retry нужно отправить буферизированные данные через новый stream_id. Новые данные от read_half тоже должны идти через новый stream_id. Используй channel (mpsc) для передачи stream_id upstream sender'у.

4. **Disconnect**: Перед retry обязательно отправлять Disconnect для старого stream_id чтобы сервер освободил TCP соединение.

5. **Логирование**: `info!()` при каждом retry attempt, `debug!()` для деталей. Формат: `"stream retry: attempt {}/6 for {}:{}"`.

6. **НЕ ломай existing flow**: HTTP (без TLS) работает отлично — первые данные приходят быстро, timeout не срабатывает, retry не нужен. Код должен быть обратно совместим.

## Constraints

- Только Rust, никаких новых зависимостей
- Только файл `crates/sp-client/src/main.rs`
- Код должен компилироваться: `cargo build --release` из `/work/vpn/slipstream-pure-dev/`
- Не менять SOCKS5 handshake
- Не менять протокол (TunnelPacket)
- Не менять server
- Не менять multipath router
- Не добавлять новые файлы

## Доступы

### VPS (сервер туннеля)
- IP: `203.0.113.130`
- Port: `22`
- User: `root`
- Password: `<REDACTED_PASSWORD>`
- SSH: `sshpass -p '<REDACTED_PASSWORD>' ssh -o StrictHostKeyChecking=no root@203.0.113.130`
- Бинарник сервера на VPS: проверь через `ssh ... 'ls /root/sp-server* || find / -name sp-server 2>/dev/null | head -5'`

### Клиент
- Запускается локально из `/work/vpn/slipstream-pure-dev/`
- Бинарник после сборки: `target/release/sp-client`

## Деплой и тестирование

### 1. Сборка
```bash
cd /work/vpn/slipstream-pure-dev && cargo build --release 2>&1
```
Должно скомпилироваться без ошибок. Warnings допустимы.

### 2. Деплой сервера (если менял — но по плану НЕ менять)
```bash
sshpass -p '<REDACTED_PASSWORD>' scp target/release/sp-server root@203.0.113.130:/root/
sshpass -p '<REDACTED_PASSWORD>' ssh root@203.0.113.130 'pkill sp-server; sleep 1; nohup /root/sp-server --domain tunnel.example.com > /root/sp-server.log 2>&1 &'
```

### 3. Запуск клиента
```bash
cd /work/vpn/slipstream-pure-dev
RUST_LOG=info,sp_client=debug target/release/sp-client --domain tunnel.example.com --config config/resolvers.toml
```

### 4. Тестирование
После запуска клиента (SOCKS5 на 127.0.0.1:7000):

**HTTP (должен работать как раньше, ~100%):**
```bash
curl -x socks5h://127.0.0.1:7000 http://httpbin.org/ip
```

**HTTPS (главный тест — должен стать ~99.8%):**
```bash
# Один запрос
curl -x socks5h://127.0.0.1:7000 https://httpbin.org/ip

# Массовый тест (10 запросов подряд, считаем успешные)
for i in $(seq 1 10); do curl -s -o /dev/null -w "%{http_code}" --max-time 35 -x socks5h://127.0.0.1:7000 https://httpbin.org/ip; echo " attempt $i"; done
```

Успех: 9-10 из 10 HTTPS запросов возвращают 200. В логах клиента видны retry attempts для некоторых запросов.

## Важно

- Не пиши комментарии с упоминанием AI/Claude
- Не создавай бэкапы — они уже есть
- Не трогай другие файлы кроме `crates/sp-client/src/main.rs`
- Делай `cargo build --release` после каждого значимого изменения для проверки компиляции
