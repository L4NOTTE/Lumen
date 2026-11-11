; Установщик Lumen (Inno Setup 6): https://jrsoftware.org/isinfo.php
; Сборка:  cargo build --release   затем   iscc installer\lumen.iss
; Результат: installer\Output\LumenSetup.exe

#define AppName "Lumen"
#define AppVersion "0.4.6"
#define AppExe "lumen.exe"
#ifndef AppSource
#define AppSource "..\target\release\lumen.exe"
#endif

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
ChangesAssociations=yes
#ifdef SignBuild
SignTool=lumen_sign
SignedUninstaller=yes
#endif

[Languages]
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"

[Tasks]
Name: "desktopicon"; Description: "Создать ярлык на рабочем столе"; GroupDescription: "Ярлыки:"

[Files]
Source: "{#AppSource}"; DestDir: "{app}"; Flags: ignoreversion
; Необязательно: положите рядом MicrosoftEdgeWebview2Setup.exe (https://developer.microsoft.com/microsoft-edge/webview2/),
; и установщик сам доустановит WebView2 на старых Windows 10.
Source: "MicrosoftEdgeWebview2Setup.exe"; DestDir: "{tmp}"; Flags: deleteafterinstall skipifsourcedoesntexist; Check: NeedsWebView2

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; Capabilities advertise support; Windows UserChoice is never overwritten.
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\Lumen"; ValueType: string; ValueName: ""; ValueData: "Lumen"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\Lumen\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"",0"
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\Lumen\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"""
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\Lumen\Capabilities"; ValueType: string; ValueName: "ApplicationName"; ValueData: "Lumen"
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\Lumen\Capabilities"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "Lumen — браузер на Rust"
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\Lumen\Capabilities"; ValueType: string; ValueName: "ApplicationIcon"; ValueData: """{app}\{#AppExe}"",0"
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\Lumen\Capabilities\StartMenu"; ValueType: string; ValueName: "StartMenuInternet"; ValueData: "Lumen"
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\Lumen\Capabilities\URLAssociations"; ValueType: string; ValueName: "http"; ValueData: "LumenURL"
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\Lumen\Capabilities\URLAssociations"; ValueType: string; ValueName: "https"; ValueData: "LumenURL"
Root: HKA; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "Lumen"; ValueData: "Software\Clients\StartMenuInternet\Lumen\Capabilities"; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\LumenURL"; ValueType: string; ValueName: ""; ValueData: "Lumen URL"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\LumenURL"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\LumenURL\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"",0"
Root: HKA; Subkey: "Software\Classes\LumenURL\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" -- ""%1"""

[Run]
Filename: "{tmp}\MicrosoftEdgeWebview2Setup.exe"; Parameters: "/silent /install"; StatusMsg: "Установка WebView2 Runtime..."; Flags: waituntilterminated skipifdoesntexist; Check: NeedsWebView2
Filename: "{app}\{#AppExe}"; Description: "Запустить {#AppName}"; Flags: nowait postinstall skipifsilent
Filename: "{app}\{#AppExe}"; Parameters: "--default-browser"; Description: "Выбрать Lumen браузером по умолчанию в настройках Windows"; Flags: nowait postinstall skipifsilent unchecked

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
