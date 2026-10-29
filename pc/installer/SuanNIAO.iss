; 蒜鸟电脑端的安装程序（Inno Setup 6）：一个 SuanNIAO-PC-版本-Setup.exe，双击装好，开始菜单（和桌面）里有「蒜鸟」。
; 只给当前用户装，装在 %LOCALAPPDATA%\Programs\SuanNIAO，不用管理员权限；卸载在 Windows 的「设置 → 应用」里。
; 发版时 .github/workflows/release.yml 编译。本机试：先在 pc 下 cargo build --release，再
;   ISCC /DAppVersion=0.7.6 pc\installer\SuanNIAO.iss
; 安装包出在 pc\target\installer\。
; 这个文件和 ChineseSimplified.isl（安装界面的中文，来自 https://github.com/kira-96/Inno-Setup-Chinese-Simplified-Translation）
; 都要存成带 BOM 的 UTF-8，不然中文是乱码。

#ifndef AppVersion
  #error 编译时要给版本号：ISCC /DAppVersion=0.7.6 SuanNIAO.iss
#endif

[Setup]
; 定了就别改：Windows 靠它认出装过的蒜鸟，覆盖安装新版、卸载都靠它
AppId={{DE1A7C69-32AD-4AD1-BF71-2B9B63532E2F}
AppName=蒜鸟
AppVersion={#AppVersion}
AppVerName=蒜鸟 {#AppVersion}
AppPublisher=SuanNIAO
VersionInfoVersion={#AppVersion}
VersionInfoProductName=蒜鸟
VersionInfoDescription=蒜鸟安装程序
DefaultDirName={autopf}\SuanNIAO
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableReadyPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
WizardStyle=modern
; 安装界面右上角的小图、「安装完成」页左边的竖图，换成蒜鸟（默认是 Inno Setup 的纸箱）。
; 竖图由 tools/logo/make_icons.py 生成
WizardSmallImageFile=..\assets\icon.png
WizardImageFile=..\assets\installer-side.png
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\SuanNIAO.exe
UninstallDisplayName=蒜鸟
OutputDir=..\target\installer
OutputBaseFilename=SuanNIAO-PC-{#AppVersion}-Setup
Compression=lzma2/max
SolidCompression=yes
; 正在运行的蒜鸟先用 --quit 关掉（见 PrepareToInstall）；还关不掉才由安装程序提示
CloseApplications=yes
RestartApplications=no
ShowLanguageDialog=no

[Languages]
Name: "chs"; MessagesFile: "ChineseSimplified.isl"

[Messages]
; 「准备安装」那页关掉了，这页的按钮就是「安装」（见 CurPageChanged），说明里别再叫人点「下一步」
SelectTasksLabel2=要不要在桌面上也放一个「[name]」图标？选好以后点「安装」。

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "..\target\release\SuanNIAO.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\SuanNIAO-core.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\蒜鸟"; Filename: "{app}\SuanNIAO.exe"
Name: "{autodesktop}\蒜鸟"; Filename: "{app}\SuanNIAO.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\SuanNIAO.exe"; Description: "打开蒜鸟"; Flags: nowait postinstall skipifsilent

[Code]
const
  RunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';
  RunValue = 'SuanNIAO';

{ 关掉装在这里的蒜鸟开着的主界面和托盘里的后台（覆盖文件、卸载之前），别的文件夹里的副本不碰。
  0.7.6 起才认 --quit；更旧的运行了反而会打开窗口，不运行 }
procedure QuitRunning();
var
  Exe: String;
  Version: Int64;
  ResultCode: Integer;
begin
  Exe := ExpandConstant('{app}\SuanNIAO.exe');
  if FileExists(Exe) and GetPackedVersion(Exe, Version) then
    if ComparePackedVersion(Version, PackVersionComponents(0, 7, 6, 0)) >= 0 then
    begin
      Log('关掉开着的蒜鸟：' + Exe + ' --quit');
      Exec(Exe, '--quit', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
      Log('关好了');
    end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  QuitRunning();
  Result := '';
end;

procedure CurPageChanged(CurPageID: Integer);
begin
  { 「准备安装」那页关掉了，在「选择附加任务」这页点了就开始装：按钮写「安装」，不写「下一步」 }
  if CurPageID = wpSelectTasks then
    WizardForm.NextButton.Caption := SetupMessage(msgButtonInstall);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Command, DataDir: String;
begin
  if CurUninstallStep = usUninstall then
  begin
    QuitRunning();
    { 开机自启（在蒜鸟里打开的，记在当前用户的 Run 里）指着这里的才删 }
    if RegQueryStringValue(HKCU, RunKey, RunValue, Command) then
      if Pos(Lowercase(ExpandConstant('{app}\')), Lowercase(Command)) > 0 then
        RegDeleteValue(HKCU, RunKey, RunValue);
  end;
  if CurUninstallStep = usPostUninstall then
  begin
    { 设置默认留着，重新装上还能接着用；问了、说要删才删。静默卸载不问、不删 }
    DataDir := ExpandConstant('{userappdata}\SuanNIAO');
    if DirExists(DataDir) and not UninstallSilent then
      if MsgBox('蒜鸟已经卸载了。要把它的设置（按键布局、配置备份、日志）也删掉吗？' + #13#10#13#10 +
                '不删的话，以后重新安装蒜鸟还能接着用。', mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
        DelTree(DataDir, True, True, True);
  end;
end;
