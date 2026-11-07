# Lumen — лаконичный браузер для Windows на Rust

Свой интерфейс (вкладки, адресная строка, стартовая страница) на `tao` + `wry`.
Страницы отображает системный WebView2 (Chromium от Microsoft).

## Возможности
- Вкладки, Ctrl+T / Ctrl+W / Ctrl+L / F5, ссылки с `target=_blank` открываются во вкладке
- Поиск: Google, Яндекс, DuckDuckGo (выбор в адресной панели, запоминается)
- Блокировка рекламы и трекеров (кнопка 🛡):
  - встроенный список доменов (Google Ads, Яндекс.Директ/Метрика, Adfox, mail.ru, VK и др.)
  - блокировка переходов на рекламные домены
  - перехват `fetch` / XHR / `sendBeacon` / динамических `script`, `iframe`, `img`
  - скрытие рекламных блоков через CSS
  - свои списки: положите `*.txt` в `%LOCALAPPDATA%\Lumen\filters\`
    (поддерживаются `||домен^`, `##селектор` и формат hosts; подойдут EasyList, AdGuard, RU AdList)
- Защита:
  - режим HTTPS-only: `http://` автоматически заменяется на `https://`
  - SmartScreen WebView2 включён
  - DevTools отключены в release-сборке
  - отдельный профиль в `%LOCALAPPDATA%\Lumen\profile`
- Оптимизация: release-профиль с LTO, `codegen-units=1`, `panic=abort`, `strip`; параллельная загрузка файлов

## Сборка
1. Установите Rust (https://rustup.rs) с целью `x86_64-pc-windows-msvc` и Visual Studio Build Tools (C++).
2. `cargo run` — запуск для отладки, `cargo build --release` — сборка (`target\release\lumen.exe`).
3. Установщик: установите Inno Setup 6, затем `iscc installer\lumen.iss` → `installer\Output\LumenSetup.exe`.
   Иконку добавьте через `SetupIconFile` в `lumen.iss`.

## Честные ограничения
- Код написан без запуска на Windows: возможны мелкие правки при первой сборке
  (API `wry` часто меняется, версии зафиксированы: `tao 0.30`, `wry 0.47`).
- Это оболочка над Chromium-движком, а не собственный движок.
- Блокировка по доменам, не по полным правилам EasyList (правила с путями и `$`-опциями пропускаются).
  Рекламу, вшитую в HTML-разметку статичными тегами, останавливает только CSS и блокировка навигации.
  Полноценная блокировка всех запросов делается через WebView2 `WebResourceRequested`
  (крейт `webview2-com`) — это следующий шаг.
- Горячие клавиши работают, когда фокус в панели интерфейса.
- Нет расширений, менеджера паролей, синхронизации, закладок и истории (хранение можно добавить через `rusqlite`).

## Сборка без установки Rust (GitHub Actions)
Загрузите проект в репозиторий GitHub (ветка `main`). Workflow `.github/workflows/build.yml`
соберёт `lumen.exe` и `LumenSetup.exe` на Windows-сервере. Скачать: вкладка Actions → последний запуск → Artifacts → `lumen-windows`.
Если сборка упадёт, пришлите текст ошибки — я исправлю код.
