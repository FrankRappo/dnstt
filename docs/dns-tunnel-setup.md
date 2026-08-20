# Настройка DNS-туннеля (DNSTT) через Server2

## Архитектура

```
Телефон (Android/iOS)
    ↓ DNS-запросы (выглядят как обычный DNS)
Yandex DNS (77.88.8.8) / Google DoH (dns.google)
    ↓ NS delegation
Server2 (203.0.113.194) — DNSTT-сервер, порт UDP 5300
    ↓ расшифровка туннеля
SOCKS5 proxy (127.0.0.1:1080)
    ↓
Интернет
```

## Требования

- **Домен** с возможностью управлять NS-записями
  - Платный: любой регистратор (~100₽/год), например reg.ru, namecheap
  - Бесплатный: freedns.afraid.org
- **Server2** (203.0.113.194) — уже есть, порт SSH: 56777
- **Телефон** — приложение DNSTT (Android) или SocksLite

---

## Шаг 1: Получить домен

### Вариант A: Бесплатный поддомен (freedns.afraid.org)

1. Зарегистрироваться на https://freedns.afraid.org
2. Перейти в Subdomains → Add
3. Создать NS-запись:
   - Type: **NS**
   - Subdomain: `t` (или любое имя)
   - Domain: выбрать из бесплатных (например `mooo.com`)
   - Destination: `203.0.113.194`
4. Итог: `t.YOURDOMAIN.mooo.com` → NS → 203.0.113.194

### Вариант B: Свой домен

1. Купить дешёвый домен (например `.xyz` за ~100₽/год)
2. В DNS-панели регистратора добавить записи:
   ```
   ns1.yourdomain.xyz  A     203.0.113.194
   t.yourdomain.xyz    NS    ns1.yourdomain.xyz
   ```
3. Итог: запросы к `*.t.yourdomain.xyz` идут на Server2

---

## Шаг 2: Установка DNSTT на Server2

```bash
# Подключаемся к Server2
ssh -p 56777 root@203.0.113.194

# Установка Go (если нет)
apt update && apt install -y golang git

# Скачиваем DNSTT
cd /opt
git clone https://www.bamsoftware.com/git/dnstt.git
cd dnstt/dnstt-server

# Сборка
go build

# Генерация ключей
./dnstt-server -gen-key -privkey-file /opt/dnstt/server.key -pubkey-file /opt/dnstt/server.pub

# Сохранить публичный ключ — понадобится для клиента!
cat /opt/dnstt/server.pub
```

---

## Шаг 3: Запуск DNSTT-сервера

### Через UDP DNS (порт 5300 + редирект с 53)

```bash
# Запуск DNSTT-сервера (проксирует на локальный SOCKS5 или HTTP)
# Вариант: проксировать на существующий Xray SOCKS
/opt/dnstt/dnstt-server/dnstt-server \
  -udp :5300 \
  -privkey-file /opt/dnstt/server.key \
  t.YOURDOMAIN.xyz \
  127.0.0.1:10443

# Если порт 53 занят (Unbound), редиректим 53 → 5300
iptables -t nat -A PREROUTING -i eth0 -p udp --dport 53 -j REDIRECT --to-port 5300
```

### Через DoH (альтернатива, не требует порт 53)

Если порт 53 занят и iptables redirect не вариант — клиент может использовать DoH-резолвер (Google, Yandex) и DNS-запросы придут через NS-делегирование.

---

## Шаг 4: Systemd-сервис

```bash
cat > /etc/systemd/system/dnstt.service << 'EOF'
[Unit]
Description=DNSTT DNS Tunnel Server
After=network.target

[Service]
Type=simple
ExecStart=/opt/dnstt/dnstt-server/dnstt-server -udp :5300 -privkey-file /opt/dnstt/server.key t.YOURDOMAIN.xyz 127.0.0.1:10443
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable dnstt
systemctl start dnstt
systemctl status dnstt
```

---

## Шаг 5: Настройка клиента на телефоне

### Android

**Приложение:** DNSTT.XYZ (из GitHub releases) или SocksLite

**Настройки:**
- Transport: **DoH** (рекомендуется) или UDP DNS
- DoH URL: `https://dns.yandex.net/dns-query` (Яндекс, разрешён МТС)
  - Альтернатива: `https://dns.google/dns-query`
- Domain: `t.YOURDOMAIN.xyz`
- Public Key: (содержимое server.pub)
- Listen: SOCKS5 на 127.0.0.1:1080

После запуска DNSTT — настроить VPN-приложение (V2rayNG, Hiddify) использовать SOCKS5 127.0.0.1:1080 как прокси.

### iOS

**Приложение:** SocksLite или Potatso Lite (с SOCKS5 прокси)

Аналогичная настройка — DNSTT как SOCKS5 прокси, VPN-клиент через него.

### Альтернатива: DNSTT + tun2socks

Для полного перенаправления трафика телефона через DNS-туннель:
1. DNSTT создаёт SOCKS5 на 127.0.0.1:1080
2. tun2socks создаёт TUN-интерфейс и направляет весь трафик через SOCKS5

---

## Шаг 6: Проверка работы

### На Server2:
```bash
# Проверить что DNSTT слушает
ss -ulnp | grep 5300

# Проверить логи
journalctl -u dnstt -f

# Проверить что DNS-запросы приходят
tcpdump -i eth0 -n udp port 5300 -c 10
```

### На телефоне:
1. Запустить DNSTT-клиент
2. Проверить SOCKS5: открыть браузер через прокси
3. Проверить IP: зайти на https://2ip.ru — должен показать IP Server2

---

## Важные моменты

### Выбор DNS-резолвера
| Резолвер | URL | Доступен через МТС? |
|----------|-----|---------------------|
| Yandex DNS | `https://dns.yandex.net/dns-query` | Вероятно да (домен Яндекса) |
| Google DoH | `https://dns.google/dns-query` | Проверить! |
| Cloudflare DoH | `https://1.1.1.1/dns-query` | Нет (заблокирован) |

**Рекомендация:** использовать Yandex DoH — IP Яндекса точно в белом списке МТС.

### Оптимизация скорости
- DoH обычно быстрее чем UDP DNS (~1.5-3 Мбит/с vs ~0.5-1.5 Мбит/с)
- Через Yandex DoH задержка минимальна (сервера в России)

### Безопасность
- DNSTT шифрует трафик end-to-end (Noise protocol)
- DNS-резолвер видит только зашифрованные DNS-запросы
- Провайдер видит только обычные DNS-запросы к Яндексу

---

## Возможные проблемы

| Проблема | Решение |
|----------|---------|
| Порт 53 на Server2 занят (Unbound) | iptables redirect 53→5300 или использовать только DoH |
| NS-записи не распространились | Подождать 1-24 часа, проверить: `dig NS t.yourdomain.xyz` |
| Клиент подключается, но нет трафика | Проверить что DNSTT проксирует на правильный порт (10443) |
| Очень медленно (<0.5 Мбит/с) | Переключиться на DoH, попробовать другой резолвер |
| DoH Яндекса недоступен | Попробовать Google DoH |

---

## Файлы и пути

- Сервер: `/opt/dnstt/` — бинарник и ключи
- Ключи: `/opt/dnstt/server.key` (приватный), `/opt/dnstt/server.pub` (публичный)
- Сервис: `/etc/systemd/system/dnstt.service`
- Клиент: приложение на телефоне (DNSTT.XYZ / SocksLite)
