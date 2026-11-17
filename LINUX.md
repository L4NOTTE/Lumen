# Lumen для Ubuntu, Fedora и Arch Linux — preview

Состояние на 18 ноября 2026 года — работа над Linux-портом приостановлена. Текущие изменения 0.5.0 проверяются на Windows. Ниже сохранены сведения о ранее подготовленной Linux-ветке.

На **Ubuntu 24.04.5, Fedora 44 и Arch Linux x86_64** локальный исходный архив прошёл нативную сборку, по 21 Rust-тесту и запуск WebKitGTK под Xvfb в виртуальных машинах. Сборка `.deb`, запуск на Linux Mint и работа в оконных менеджерах остаются отдельными проверками. Linux-бинарник в архив исходников не включён.

Workflow `build-linux` собирает пакеты на Ubuntu 24.04. Mint 22.x использует пакетную базу Ubuntu 24.04. [Сведения Linux Mint](https://linuxmint.com/rel_wilma_whatsnew.php).

## Возможности и отличия

WebKit Inspector доступен из меню. Запросы разрешений сайтов, включая камеру и микрофон, сейчас отклоняются. Пользовательская установка в `~/.local/bin/lumen` проверяет подписанные обновления через `/usr/bin/curl`. Системный пакет обновляется через менеджер пакетов.

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
| Восстановление сеанса | Ленивое создание фоновых вкладок | До 50 страниц. При запуске загружается активная вкладка, остальные — при первом выборе. |
| Рамка окна | Собственная Windows | Нативная GTK/оконного менеджера |

Для glib 0.18.5 включено исправление RUSTSEC-2024-0429. Тест исправления входит в нативный набор. Подробности — в [SECURITY.md](SECURITY.md).

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

Workflow `build-linux` содержит сборку и Rust-тесты для Fedora 44 и Arch Linux с системным WebKitGTK. Графический тест и сборка `.deb` выполняются отдельно на Ubuntu 24.04. Результаты работы в оконном менеджере Fedora и Arch нужно проверять на этих системах.

Для сборки из исходников на Fedora 44:

```bash
sudo dnf install git curl gcc pkgconf-pkg-config gtk3-devel webkit2gtk4.1-devel libX11-devel libXrandr-devel libxkbcommon-devel wayland-devel
```

Для сборки на Arch Linux:

```bash
sudo pacman -Syu --needed base-devel git curl pkgconf gtk3 webkit2gtk-4.1 libx11 libxrandr libxkbcommon wayland
```

Затем на любом из поддерживаемых стендов проверьте зависимости командой `bash tools/install-linux.sh --check` и выполните установку командой `bash tools/install-linux.sh`.

### Установка без интернета

Для Ubuntu 24.04 x86_64 есть отдельный комплект с готовым браузером и системными пакетами WebKitGTK/GTK. Распакуйте архив полностью и выполните `bash install-offline.sh`. Установка использует только пакеты из архива и проверяет SHA-256. Cargo для запуска готового браузера не нужен.

Архив с исходниками содержит снимок зависимостей Cargo в `vendor/registry`. При наличии Rust stable и системных пакетов разработки выполните `bash tools/install-linux.sh --offline`. Сам компилятор Rust и системные пакеты разработки устанавливаются отдельно.

### Установка из GitHub через Git

Этот способ собирает текущую Linux preview на WebKitGTK. Он **не добавляет** поддержку Chrome-расширений: отдельный Chromium-прототип ещё проходит проверку совместимости.

На Ubuntu 24.04 / Linux Mint 22 сначала установите инструменты и системные библиотеки:

```bash
sudo apt update
sudo apt install git curl build-essential pkg-config libgtk-3-dev libwebkit2gtk-4.1-dev libx11-dev libxrandr-dev libxkbcommon-dev libwayland-dev gstreamer1.0-libav gstreamer1.0-plugins-good
```

Нужны установленные `cargo` и `rustc` (Rust stable). Если их нет, установите Rust по инструкции https://rustup.rs/ и откройте терминал заново. Если библиотеки уже установлены, последующие команды не требуют sudo:

```bash
git clone https://github.com/L4NOTTE/Lumen.git
cd Lumen
bash tools/install-linux.sh --check
bash tools/install-linux.sh
```

Скрипт собирает Rust-код с `--locked`, устанавливает бинарник в `~/.local/bin/lumen`, иконку и пункт меню — в `~/.local/share`. Профиль и браузер по умолчанию не меняются. Запуск: меню **Lumen** или `~/.local/bin/lumen`.

Обновление из того же каталога:

```bash
git pull --ff-only
bash tools/install-linux.sh
```

Сначала закройте Lumen. Если Git сообщает о ваших локальных изменениях, сохраните их перед обновлением; `reset --hard` не нужен. Не запускайте сборку через sudo.

Скрипт должен быть загружен в репозиторий вместе с исходниками. До этого `git clone` скачает текущую опубликованную версию без нового установщика. Нативный запуск этого установщика здесь не проверен (Windows без WSL); он требует проверки на Ubuntu/Mint.

### Ручная сборка и пакет

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
