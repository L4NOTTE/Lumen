# Подпись Windows-сборок Lumen

Текущий выпуск собирается **без цифровой подписи**: сертификат добавим позднее. Поле AppPublisher в установщике — описание приложения, оно не заменяет Authenticode. Самоподписанный сертификат не добавляет публичного доверия Windows; отключать SmartScreen или удалять метки загрузки для распространения не требуется.

Для выпуска с проверенным издателем нужен доверенный Code Signing certificate с приватным ключом либо настроенный сервис подписания. Подпись подтверждает издателя и целостность. **Она не гарантирует исчезновение SmartScreen:** Windows также проверяет репутацию файла и издателя. [Microsoft: SmartScreen reputation](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation).

## Уже подготовлено

- `tools/sign-windows.ps1`: SHA-256, RFC3161 timestamp, проверка доверенной цепочки, Code Signing EKU и результата SignTool/Authenticode. Без конфигурации сообщает о неподписанной сборке. Если подпись обязательна или конфигурация неполная — завершается ошибкой.
- `tools/build-installer.ps1`: при заданном сертификате включает SignTool самого Inno Setup. Подписываются Setup, деинсталлятор и временные копии Setup, а не только внешний EXE после сборки. [Документация Inno Setup](https://jrsoftware.org/ishelp/topic_setup_signtool.htm).
- Workflow `build-windows`: подписывает браузер перед упаковкой; передаёт конфигурацию подписания установщику. Без секретов собирает обычный неподписанный выпуск. Компилятор Inno Setup 6.7.4 скачивается с официального GitHub, его Authenticode проверяется перед запуском.

## Когда появится экспортируемый PFX

В репозитории GitHub откройте **Settings → Secrets and variables → Actions** и задайте:

| Secret | Значение |
| --- | --- |
| `LUMEN_SIGN_PFX_BASE64` | PFX-файл, преобразованный в Base64 |
| `LUMEN_SIGN_PFX_PASSWORD` | Пароль PFX |

Base64 не шифрует приватный ключ. Его нельзя добавлять в исходники, README, чат или обычные repository variables. Используйте только Actions secrets и доверенные workflow. На одноразовом Windows runner PFX импортируется в CurrentUser\My; временный PFX и новые импортированные сертификаты удаляются после подписания. Пароль не передаётся аргументом командной строки SignTool.

Если сертификат находится в аппаратном токене и не экспортируется в PFX, либо выбран Microsoft Artifact Signing, этот PFX-процесс не подходит: потребуется подключить провайдер/облачное действие отдельно. Возможность такой схемы здесь не объявляется реализованной.

## Локальное подписание из хранилища Windows

После установки Windows SDK и настоящего сертификата с доступным приватным ключом:

```powershell
$env:LUMEN_SIGN_CERT_SHA1 = 'THUMBPRINT_ИЗ_ХРАНИЛИЩА_СЕРТИФИКАТОВ'
./tools/sign-windows.ps1 -RequireSignature -Files ./target/release/lumen.exe
./tools/build-installer.ps1
```

SHA1 здесь — идентификатор сертификата; подпись файлов использует SHA-256. По умолчанию задан HTTPS timestamp-сервер DigiCert, его можно заменить совместимым RFC3161 URL через `LUMEN_TIMESTAMP_URL`.

С настоящим сертификатом подписание и timestamp этого проекта пока не проверялись. Не следует считать выпуск подписанным только потому, что в нём есть этот скрипт. Проверка результата:

```powershell
Get-AuthenticodeSignature ./target/release/lumen.exe
Get-AuthenticodeSignature ./installer/Output/LumenSetup.exe
```
