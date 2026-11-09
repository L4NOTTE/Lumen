; Установщик Lumen (Inno Setup 6): https://jrsoftware.org/isinfo.php
; Сборка:  cargo build --release   затем   iscc installer\lumen.iss
; Результат: installer\Output\LumenSetup.exe

#define AppName "Lumen"
#define AppVersion "0.1.0"
#define AppExe "lumen.exe"

[Setup]
AppId={{B7C1E3A4-5D2F-4C8E-9A61-3F0E8D7B2A19}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=Lumen
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
OutputDir=Output
OutputBaseFilename=LumenSetup
Compression=lzma2/ultra
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
WizardStyle=modern
UninstallDisplayIcon={app}\{#AppExe}
SetupIconFile=..\assets\lumen.ico

[Languages]
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"

[Tasks]
Name: "desktopicon"; Description: "Создать ярлык на рабочем столе"; GroupDescription: "Ярлыки:"

[Files]
Source: "..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
; Необязательно: положите рядом MicrosoftEdgeWebview2Setup.exe (https://developer.microsoft.com/microsoft-edge/webview2/),
; и установщик сам доустановит WebView2 на старых Windows 10.
Source: "MicrosoftEdgeWebview2Setup.exe"; DestDir: "{tmp}"; Flags: deleteafterinstall skipifsourcedoesntexist; Check: NeedsWebView2

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
Filename: "{tmp}\MicrosoftEdgeWebview2Setup.exe"; Parameters: "/silent /install"; StatusMsg: "Установка WebView2 Runtime..."; Flags: waituntilterminated skipifdoesntexist; Check: NeedsWebView2
Filename: "{app}\{#AppExe}"; Description: "Запустить {#AppName}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; Профиль (история, куки, настройки) лежит в %LOCALAPPDATA%\Lumen — удаляется только по желанию пользователя.

[Code]
function WebView2Installed(): Boolean;
var
  v: String;
begin
  Result :=
    RegQueryStringValue(HKLM, 'SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}', 'pv', v) or
    RegQueryStringValue(HKCU, 'Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}', 'pv', v);
  if Result then Result := (v <> '') and (v <> '0.0.0.0');
end;

function NeedsWebView2(): Boolean;
begin
  Result := not WebView2Installed();
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    if MsgBox('Удалить также данные браузера (историю, куки, настройки)?', mbConfirmation, MB_YESNO) = IDYES then
      DelTree(ExpandConstant('{localappdata}\Lumen'), True, True, True);
end;
