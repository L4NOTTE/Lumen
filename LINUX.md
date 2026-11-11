# Lumen для Ubuntu / Linux Mint — preview

Подготовлен первый Linux-порт для **Ubuntu 24.04 x86_64 / Linux Mint 22.x x86_64**. Здесь проверены Rust-типы Linux-ветки, но **нативная линковка, запуск в Linux и сборка `.deb` локально не выполнялись**: рабочая машина — Windows без установленного WSL/Linux. Linux-бинарник в архив исходников не включён.

Пакет считается собранным и прошедшим проверки только после зелёного workflow `build-linux`. Mint 22.x использует пакетную базу Ubuntu 24.04. [Сведения Linux Mint](https://linuxmint.com/rel_wilma_whatsnew.php).

## Возможности и отличия

Перенесены общий интерфейс, вкладки и плавная перестановка внутри окна, адресная строка и поиск, история, быстрые ссылки, темы/шрифты/фон, масштаб по сайтам, восстановление сеанса и инкогнито. Добавлена обработка fullscreen-событий страниц и размещение WebKit Inspector в боковой GTK-панели.

| Возможность | Windows | Linux preview |
| --- | --- | --- |
| Движок | WebView2 / Chromium | Системный WebKitGTK 4.1 |
| Раздел загрузок / Ctrl + J | Нативные загрузки WebView2 | Адаптер WebKitGTK; требуется проверка в Linux |
| Chrome-расширения, Shazam, VK Next | Экспериментальная поддержка | Не реализованы |
| DevTools | Chromium DevTools в оформлении Lumen | Нативный WebKit Inspector, другие интерфейс и инструменты |
| Блокировка рекламы | Сетевые и косметические фильтры | Базовые JS/CSS-фильтры; не перехватывают все сетевые запросы |
| Усыпление / эконом-режим Chromium | Есть | Не перенесены; настройки скрыты |
| Перенос вкладок между окнами | Передача адреса | Не реализован |
| Восстановление сеанса | Ленивое создание фоновых вкладок | До 50 страниц без ленивой загрузки |
| Рамка окна | Собственная Windows | Нативная GTK/оконного менеджера |

Перед признанием Linux-порта готовым нужно разобраться с advisory glib 0.18.5 (RUSTSEC-2024-0429) и выполнить нативные проверки. Подробности — в [SECURITY.md](SECURITY.md).

Полного равенства версий пока нет. Не проверены реальные сайты, видео/DRM, камера/микрофон, Wayland и конкретные оконные менеджеры. GTK-контейнеры раскладывают панель, страницы и инспектор; выбран `build_gtk`, подходящий для X11 и Wayland. [Документация Wry 0.47.2](https://docs.rs/wry/0.47.2/wry/).

## Получить `.deb` через GitHub

1. Загрузите **содержимое** архива исходников в корень репозитория, включая `.github`, а не сам ZIP.
2. Откройте **Actions → build-linux → Run workflow**. Push в `main` также запускает сборку.
3. Workflow установит GTK/WebKitGTK, выполнит Rust/JS-тесты, соберёт приложение и запустит графический тест GTK + WebKit в Xvfb. WebKit sandbox не отключается.
4. После зелёного завершения скачайте артефакт **lumen-linux-ubuntu24-amd64**: `.deb`, `.tar.gz`, `SHA256SUMS`.
5. На Ubuntu/Mint установите пакет:

```bash
sudo apt install ./lumen_0.4.6_amd64.deb
```

В меню появится **Lumen**. Браузер по умолчанию автоматически не меняется. При сбое графического теста workflow сохраняет журнал `lumen-linux-diagnostics`; успешный пакет при ошибке не публикуется. Windows собирается отдельно через `build-windows`.

## Локальная сборка

Нужен Rust stable и пакеты разработки GTK 3 / WebKitGTK 4.1. [Пакет WebKitGTK для Ubuntu 24.04](https://packages.ubuntu.com/en/noble-updates/libwebkit2gtk-4.1-dev).

```bash
sudo apt update
sudo apt install build-essential pkg-config libgtk-3-dev libwebkit2gtk-4.1-dev libx11-dev libxrandr-dev libxkbcommon-dev libwayland-dev dpkg-dev
cargo test --locked
cargo build --release --locked
./target/release/lumen
bash tools/package-linux.sh
```

Пакеты появятся в `dist/`. Зависимости `.deb` определяются `dpkg-shlibdeps`. Для мультимедиа пакет рекомендует `gstreamer1.0-libav` и `gstreamer1.0-plugins-good`. Архив `.tar.gz` **не автономный**: нужны совместимые GTK/WebKitGTK и glibc. Он рассчитан на ту же базу Ubuntu 24.04/Mint 22, а не любой Linux.

Для графического теста установите `xvfb dbus-x11` и запустите приложение с `--smoke-test` через `xvfb-run -a dbus-run-session -- timeout 45s`. Задайте отдельный новый каталог `LUMEN_DATA_DIR`. Полная команда есть в `.github/workflows/build-linux.yml`.

Строка `LUMEN_LINUX_SMOKE_OK` появляется только после ответа настоящего интерфейса: проверяются создание вкладок, масштаб, перестановка и DOM панели. Без ответа процесс завершается с ошибкой по таймауту.

## Профиль

- Настройки, ссылки, история: `~/.config/lumen/` с учётом XDG-переменных.
- Данные сайтов: `~/.local/share/lumen/webkit/` с учётом XDG-переменных.
- `LUMEN_DATA_DIR` задаёт отдельный каталог для тестов.
- Инкогнито использует непостоянный WebKit-контекст и не записывает историю/настройки в основной профиль.

Профиль Windows/WebView2 и его расширения не переносятся простым копированием в Linux.
