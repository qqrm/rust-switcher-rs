; Shared per-user application lifecycle. Include after [Code].
; Required defines: AppName, AppExeName, PackageIdentifier, RunValueName,
; AppId, RestartArguments, AutostartArguments, DefaultInstallDirectory.
; CloseApplications and RestartApplications must both be disabled: legacy
; portable releases do not register with Windows Restart Manager.
#ifndef AdditionalPayloadName
  #define AdditionalPayloadName ""
#endif

const
  LifecycleUninstallRoot = 'Software\Microsoft\Windows\CurrentVersion\Uninstall';
  LifecycleRunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';
  LifecycleQuery = $1000;
  LifecycleSynchronize = $100000;
  LifecycleTerminate = $0001;
  LifecycleWaitTimeout = 258;
  LifecycleOfficialSource = 'Microsoft.Winget.Source_8wekyb3d8bbwe';

type
  TLifecycleProcessIds = array[0..8191] of Cardinal;
  TLifecycleHandles = array of THandle;
  TLifecycleTokenStatistics = array[0..13] of Cardinal;
  TLifecycleTokenPrivileges = array[0..255] of Cardinal;
  TLifecycleLuid = record LowPart: Cardinal; HighPart: Integer; end;
  TLifecycleStartupInfo = record
    Size, Reserved, Desktop, Title: Cardinal;
    X, Y, XSize, YSize, XCount, YCount, FillAttribute, Flags: Cardinal;
    ShowWindow, ReservedSize: Word;
    ReservedBytes, StandardInput, StandardOutput, StandardError: Cardinal;
  end;
  TLifecycleProcessInformation = record
    Process, Thread: THandle;
    ProcessId, ThreadId: Cardinal;
  end;

function LifecycleEnumProcesses(var Ids: TLifecycleProcessIds; Bytes: Cardinal;
  var Needed: Cardinal): Boolean;
  external 'EnumProcesses@psapi.dll stdcall';
function LifecycleOpenProcess(Access: Cardinal; Inherit: Boolean; Id: Cardinal): THandle;
  external 'OpenProcess@kernel32.dll stdcall';
function LifecycleCloseHandle(Handle: THandle): Boolean;
  external 'CloseHandle@kernel32.dll stdcall';
function LifecycleQueryImage(Handle: THandle; Flags: Cardinal; Buffer: String;
  var Size: Cardinal): Boolean;
  external 'QueryFullProcessImageNameW@kernel32.dll stdcall';
function LifecycleProcessSession(Id: Cardinal; var Session: Cardinal): Boolean;
  external 'ProcessIdToSessionId@kernel32.dll stdcall';
function LifecycleCurrentProcessId: Cardinal;
  external 'GetCurrentProcessId@kernel32.dll stdcall';
function LifecycleWait(Handle: THandle; Milliseconds: Cardinal): Cardinal;
  external 'WaitForSingleObject@kernel32.dll stdcall';
function LifecycleKill(Handle: THandle; ExitCode: Cardinal): Boolean;
  external 'TerminateProcess@kernel32.dll stdcall';
{ The build uses the hosted Inno 6 compiler, whose setup process is 32-bit. }
function LifecycleOpenFile(Name: String; Access, Share: Cardinal;
  Security: Cardinal; Creation, Flags: Cardinal; Template: THandle): THandle;
  external 'CreateFileW@kernel32.dll stdcall';
function LifecycleFinalPath(Handle: THandle; Buffer: String; Size, Flags: Cardinal): Cardinal;
  external 'GetFinalPathNameByHandleW@kernel32.dll stdcall';
function LifecycleFileAttributes(Name: String): Cardinal;
  external 'GetFileAttributesW@kernel32.dll stdcall';
function LifecyclePathIsRelative(Name: String): Boolean;
  external 'PathIsRelativeW@shlwapi.dll stdcall';
function LifecycleOpenKey(Root: Cardinal; Subkey: String; Options, Access: Cardinal;
  var Key: THandle): Integer;
  external 'RegOpenKeyExW@advapi32.dll stdcall';
function LifecycleQueryValue(Key: THandle; Name: String; Reserved: Cardinal;
  var ValueType: Cardinal; Data: Cardinal; var Size: Cardinal): Integer;
  external 'RegQueryValueExW@advapi32.dll stdcall';
function LifecycleCloseKey(Key: THandle): Integer;
  external 'RegCloseKey@advapi32.dll stdcall';
function LifecycleOpenToken(Process: THandle; Access: Cardinal; var Token: THandle): Boolean;
  external 'OpenProcessToken@advapi32.dll stdcall';
function LifecycleTokenDword(Token: THandle; Information: Integer; var Value: Cardinal;
  Size: Cardinal; var Needed: Cardinal): Boolean;
  external 'GetTokenInformation@advapi32.dll stdcall';
function LifecycleTokenStatistics(Token: THandle; Information: Integer;
  var Value: TLifecycleTokenStatistics; Size: Cardinal; var Needed: Cardinal): Boolean;
  external 'GetTokenInformation@advapi32.dll stdcall';
function LifecycleTokenPrivileges(Token: THandle; Information: Integer;
  var Value: TLifecycleTokenPrivileges; Size: Cardinal; var Needed: Cardinal): Boolean;
  external 'GetTokenInformation@advapi32.dll stdcall';
function LifecycleLookupPrivilege(System: Cardinal; Name: String; var Luid: TLifecycleLuid): Boolean;
  external 'LookupPrivilegeValueW@advapi32.dll stdcall';
function LifecycleDuplicateToken(Token: THandle; Access, Attributes: Cardinal;
  Level, TokenType: Integer; var Duplicate: THandle): Boolean;
  external 'DuplicateTokenEx@advapi32.dll stdcall';
function LifecycleEnvironment: Cardinal;
  external 'GetEnvironmentStringsW@kernel32.dll stdcall';
function LifecycleFreeEnvironment(Environment: Cardinal): Boolean;
  external 'FreeEnvironmentStringsW@kernel32.dll stdcall';
function LifecycleCreateWithToken(Token: THandle; LogonFlags: Cardinal;
  Application, Command: String; CreationFlags, Environment: Cardinal; Directory: String;
  var Startup: TLifecycleStartupInfo; var Process: TLifecycleProcessInformation): Boolean;
  external 'CreateProcessWithTokenW@advapi32.dll stdcall';

var
  LifecycleLegacyKey, LifecycleLegacyDirectory, LifecycleLegacyTarget: String;
  LifecycleLegacyLink, LifecycleTarget, LifecycleBackup, LifecycleOldHash: String;
  LifecycleExtraTarget, LifecycleExtraBackup, LifecycleExtraHash: String;
  LifecycleOldInnoVersion: String;
  LifecycleDetected, LifecyclePrepared, LifecycleWasRunning: Boolean;
  LifecycleCompleted, LifecycleRestarted, LifecycleOldInnoExists: Boolean;
  LifecycleExtraExisted, LifecycleLegacyPathAdded: Boolean;
  LifecycleLaunchToken: THandle;
  LifecycleHandles: TLifecycleHandles;

function LifecycleOwnKey: String;
begin
  Result := LifecycleUninstallRoot + '\{#AppId}_is1';
end;

function LifecycleNormalizedPath(const Path: String): String;
begin
  Result := RemoveBackslashUnlessRoot(ExpandFileName(Path));
end;

function LifecycleCanonicalPath(const Path: String): String;
var
  Handle: THandle;
  Buffer: String;
  Size: Cardinal;
begin
  Result := '';
  Handle := LifecycleOpenFile(Path, 0, 7, 0, 3, $02000000, 0);
  if Handle = THandle(-1) then Exit;
  try
    SetLength(Buffer, 32768);
    Size := LifecycleFinalPath(Handle, Buffer, 32768, 0);
    if (Size = 0) or (Size >= 32768) then Exit;
    SetLength(Buffer, Size);
    if Copy(Buffer, 1, 8) = '\\?\UNC\' then
      Buffer := '\\' + Copy(Buffer, 9, Length(Buffer))
    else if Copy(Buffer, 1, 4) = '\\?\' then
      Delete(Buffer, 1, 4);
    Result := LifecycleNormalizedPath(Buffer);
  finally
    LifecycleCloseHandle(Handle);
  end;
end;

function LifecycleSamePath(const Left, Right: String): Boolean;
var
  CanonicalLeft, CanonicalRight: String;
begin
  CanonicalLeft := LifecycleCanonicalPath(Left);
  CanonicalRight := LifecycleCanonicalPath(Right);
  Result := (CanonicalLeft <> '') and (CanonicalRight <> '') and
    (CompareText(CanonicalLeft, CanonicalRight) = 0);
end;

procedure LifecycleDiscoverLegacy;
var
  Names: TArrayOfString;
  Index: Integer;
  PathAdded: Cardinal;
  Key, PackageId, SourceId, InstallerType, Directory, Target: String;
begin
  if LifecycleDetected then Exit;
  LifecycleDetected := True;
  if not RegGetSubkeyNames(HKCU64, LifecycleUninstallRoot, Names) then Exit;
  for Index := 0 to GetArrayLength(Names) - 1 do begin
    Key := LifecycleUninstallRoot + '\' + Names[Index];
    if not RegQueryStringValue(HKCU64, Key, 'WinGetPackageIdentifier', PackageId) then Continue;
    if CompareText(PackageId, '{#PackageIdentifier}') <> 0 then Continue;
    if not RegQueryStringValue(HKCU64, Key, 'WinGetSourceIdentifier', SourceId) then Continue;
    if SourceId <> LifecycleOfficialSource then Continue;
    if CompareText(Names[Index], '{#PackageIdentifier}_' + SourceId) <> 0 then Continue;
    if not RegQueryStringValue(HKCU64, Key, 'WinGetInstallerType', InstallerType) then Continue;
    if InstallerType <> 'portable' then Continue;
    if not RegQueryStringValue(HKCU64, Key, 'InstallLocation', Directory) or
       not RegQueryStringValue(HKCU64, Key, 'TargetFullPath', Target) then
      RaiseException('The existing WinGet registration has no installation path.');
    if (Directory = '') or (Target = '') or LifecyclePathIsRelative(Directory) or
       LifecyclePathIsRelative(Target) or
       (CompareText(LifecycleNormalizedPath(Target),
         AddBackslash(LifecycleNormalizedPath(Directory)) + '{#AppExeName}') <> 0) or
       not FileExists(Target) then
      RaiseException('The existing WinGet registration has an invalid executable path.');
    if LifecycleLegacyKey <> '' then
      RaiseException('More than one portable installation is registered for this user.');
    LifecycleLegacyKey := Key;
    LifecycleLegacyDirectory := LifecycleNormalizedPath(Directory);
    LifecycleLegacyTarget := LifecycleNormalizedPath(Target);
    RegQueryStringValue(HKCU64, Key, 'SymlinkFullPath', LifecycleLegacyLink);
    LifecycleLegacyPathAdded := RegQueryDWordValue(HKCU64, Key,
      'InstallDirectoryAddedToPath', PathAdded) and (PathAdded <> 0);
    Log('Validated portable installation: ' + LifecycleLegacyTarget);
  end;
end;

function GetDefaultInstallDirectory(Param: String): String;
begin
  LifecycleDiscoverLegacy;
  if LifecycleLegacyDirectory <> '' then Result := LifecycleLegacyDirectory
  else Result := ExpandConstant('{#DefaultInstallDirectory}');
end;

procedure LifecycleReleaseHandles(var Handles: TLifecycleHandles);
var
  Index: Integer;
begin
  for Index := 0 to GetArrayLength(Handles) - 1 do LifecycleCloseHandle(Handles[Index]);
  SetArrayLength(Handles, 0);
end;

procedure LifecycleCapture(const Target: String; var Handles: TLifecycleHandles);
var
  Ids: TLifecycleProcessIds;
  Needed, Session, CurrentSession, Size, WaitResult: Cardinal;
  Index, Count: Integer;
  Handle: THandle;
  Buffer, CanonicalTarget: String;
begin
  SetArrayLength(Handles, 0);
  CanonicalTarget := LifecycleCanonicalPath(Target);
  if CanonicalTarget = '' then Exit;
  if not LifecycleProcessSession(LifecycleCurrentProcessId, CurrentSession) then
    RaiseException('Cannot identify the current Windows session.');
  if not LifecycleEnumProcesses(Ids, 32768, Needed) or (Needed >= 32768) then
    RaiseException('Cannot take a complete process snapshot.');
  try
    for Index := 0 to (Needed div 4) - 1 do begin
      if not LifecycleProcessSession(Ids[Index], Session) or (Session <> CurrentSession) then Continue;
      Handle := LifecycleOpenProcess(LifecycleQuery or LifecycleSynchronize or LifecycleTerminate,
        False, Ids[Index]);
      if Handle = 0 then Continue;
      SetLength(Buffer, 32768);
      Size := 32768;
      if LifecycleQueryImage(Handle, 0, Buffer, Size) then begin
        SetLength(Buffer, Size);
        if CompareText(LifecycleCanonicalPath(Buffer), CanonicalTarget) = 0 then begin
          WaitResult := LifecycleWait(Handle, 0);
          if (WaitResult <> 0) and (WaitResult <> LifecycleWaitTimeout) then begin
            LifecycleCloseHandle(Handle);
            RaiseException('Cannot inspect the captured application process.');
          end;
          if WaitResult = LifecycleWaitTimeout then begin
            Count := GetArrayLength(Handles);
            SetArrayLength(Handles, Count + 1);
            Handles[Count] := Handle;
            Handle := 0;
          end;
        end;
      end;
      if Handle <> 0 then LifecycleCloseHandle(Handle);
    end;
  except
    LifecycleReleaseHandles(Handles);
    RaiseException(GetExceptionMessage);
  end;
end;

procedure LifecycleStop(const Target: String; var Handles: TLifecycleHandles);
var
  Index, ResultCode: Integer;
  WaitResult: Cardinal;
begin
  if GetArrayLength(Handles) = 0 then Exit;
  { The command is the production graceful shutdown route, including old portable
    versions. Only captured exact-path/current-session handles can be forced. }
  if not Exec(Target, '--quit-for-update', ExtractFileDir(Target), SW_HIDE, ewNoWait, ResultCode) then
    Log('Graceful shutdown helper could not start: ' + SysErrorMessage(ResultCode));
  for Index := 0 to GetArrayLength(Handles) - 1 do begin
    WaitResult := LifecycleWait(Handles[Index], 10000);
    if WaitResult = LifecycleWaitTimeout then begin
      Log('Graceful shutdown timed out; stopping the captured application handle.');
      if not LifecycleKill(Handles[Index], 0) then
        RaiseException('The running application could not be stopped.');
      if LifecycleWait(Handles[Index], 5000) <> 0 then
        RaiseException('The running application did not exit before the update deadline.');
    end else if WaitResult <> 0 then
      RaiseException('Waiting for the captured application failed.');
  end;
end;

function LifecycleSameLogon(Left, Right: THandle): Boolean;
var
  LeftStats, RightStats: TLifecycleTokenStatistics;
  Needed: Cardinal;
begin
  if not LifecycleTokenStatistics(Left, 10, LeftStats, 56, Needed) or
    not LifecycleTokenStatistics(Right, 10, RightStats, 56, Needed) then
    RaiseException('Cannot compare application and installer logon identities.');
  { AuthenticationId is the LUID at byte offset 8; no native SID pointer is
    retained across Pascal Script's DLL marshalling boundary. }
  Result := (LeftStats[2] = RightStats[2]) and (LeftStats[3] = RightStats[3]);
end;

procedure LifecycleRetainLaunchToken(ApplicationProcess: THandle);
var
  OwnToken, ApplicationToken, LinkedToken: THandle;
  Elevated, Needed, Index, LinkedTokenValue: Cardinal;
  Privileges: TLifecycleTokenPrivileges;
  Impersonate: TLifecycleLuid;
  Allowed, SameUser: Boolean;
begin
  if not LifecycleOpenToken(THandle(-1), 8, OwnToken) then
    RaiseException('Cannot inspect the installer security token.');
  try
    if not LifecycleTokenDword(OwnToken, 20, Elevated, 4, Needed) then
      RaiseException('Cannot inspect installer elevation.');
    if Elevated = 0 then Exit;
    if not LifecycleOpenToken(ApplicationProcess, $B, ApplicationToken) then
      RaiseException('Cannot retain the running application security token.');
    try
      SameUser := LifecycleSameLogon(OwnToken, ApplicationToken);
      { UAC split tokens can identify different linked logon sessions. Only
        Windows' own linked token is accepted, never an unrelated user's token. }
      if not SameUser and LifecycleTokenDword(OwnToken, 19, LinkedTokenValue, 4, Needed) then begin
        LinkedToken := THandle(LinkedTokenValue);
        try SameUser := LifecycleSameLogon(LinkedToken, ApplicationToken);
        finally LifecycleCloseHandle(LinkedToken); end;
      end;
      if not SameUser then
        RaiseException('The running application belongs to another Windows logon.');
      if not LifecycleLookupPrivilege(0, 'SeImpersonatePrivilege', Impersonate) or
        not LifecycleTokenPrivileges(OwnToken, 3, Privileges, 1024, Needed) then
        RaiseException('Cannot verify the privilege needed to preserve application elevation.');
      if (Privileges[0] > 85) or (Needed > 1024) then
        RaiseException('The installer privilege snapshot is incomplete.');
      Allowed := False;
      if Privileges[0] > 0 then
        for Index := 0 to Privileges[0] - 1 do
          if (Privileges[1 + Index * 3] = Impersonate.LowPart) and
            (Privileges[2 + Index * 3] = Cardinal(Impersonate.HighPart)) and
            ((Privileges[3 + Index * 3] and 2) <> 0) then Allowed := True;
      if not Allowed then
        RaiseException('The installer cannot restart the application with its original privileges.');
      if not LifecycleDuplicateToken(ApplicationToken, $B, 0, 2, 1, LifecycleLaunchToken) then
        RaiseException('The running application primary token could not be duplicated.');
    finally
      LifecycleCloseHandle(ApplicationToken);
    end;
  finally
    LifecycleCloseHandle(OwnToken);
  end;
end;

function LifecycleStartWithToken(const Target: String): Boolean;
var
  Environment: Cardinal;
  Startup: TLifecycleStartupInfo;
  Process: TLifecycleProcessInformation;
begin
  Result := False;
  Environment := LifecycleEnvironment;
  if Environment = 0 then Exit;
  try
    Startup.Size := 68;
    Startup.Reserved := 0; Startup.Desktop := 0; Startup.Title := 0;
    Startup.X := 0; Startup.Y := 0; Startup.XSize := 0; Startup.YSize := 0;
    Startup.XCount := 0; Startup.YCount := 0; Startup.FillAttribute := 0; Startup.Flags := 0;
    Startup.ShowWindow := 0; Startup.ReservedSize := 0; Startup.ReservedBytes := 0;
    Startup.StandardInput := 0; Startup.StandardOutput := 0; Startup.StandardError := 0;
    { The copied Unicode environment preserves APPDATA and the caller's other
      per-user settings while the retained token preserves the app's privileges. }
    if not LifecycleCreateWithToken(LifecycleLaunchToken, 0, Target,
        '"' + Target + '" {#RestartArguments}', $400, Environment,
        ExtractFileDir(Target), Startup, Process) then Exit;
    try
      Result := LifecycleWait(Process.Process, 1000) = LifecycleWaitTimeout;
    finally
      LifecycleCloseHandle(Process.Thread);
      LifecycleCloseHandle(Process.Process);
    end;
  finally
    LifecycleFreeEnvironment(Environment);
  end;
end;

function LifecycleStart(const Target: String): Boolean;
var
  ResultCode, Attempt: Integer;
  Handles: TLifecycleHandles;
begin
  Result := False;
  if not FileExists(Target) then Exit;
  if LifecycleLaunchToken <> 0 then begin
    Result := LifecycleStartWithToken(Target);
    Exit;
  end;
  { Use the original user's credentials, not an elevated installer child. }
  if not ExecAsOriginalUser(Target, '{#RestartArguments}', ExtractFileDir(Target),
      SW_SHOWNORMAL, ewNoWait, ResultCode) then begin
    Log('Restart failed: ' + SysErrorMessage(ResultCode));
    Exit;
  end;
  for Attempt := 1 to 100 do begin
    LifecycleCapture(Target, Handles);
    try
      if GetArrayLength(Handles) > 0 then begin
        Result := LifecycleWait(Handles[0], 1000) = LifecycleWaitTimeout;
        Exit;
      end;
    finally
      LifecycleReleaseHandles(Handles);
    end;
    Sleep(50);
  end;
end;

function LifecycleRemovePathSegment(const Value, Directory: String): String;
var
  Remaining, Segment, Comparable: String;
  Delimiter: Integer;
  First: Boolean;
begin
  Result := '';
  Remaining := Value;
  First := True;
  repeat
    Delimiter := Pos(';', Remaining);
    if Delimiter = 0 then begin Segment := Remaining; Remaining := ''; end
    else begin Segment := Copy(Remaining, 1, Delimiter - 1); Delete(Remaining, 1, Delimiter); end;
    Comparable := Trim(Segment);
    if (Length(Comparable) >= 2) and (Comparable[1] = '"') and
      (Comparable[Length(Comparable)] = '"') then
      Comparable := Copy(Comparable, 2, Length(Comparable) - 2);
    if CompareText(RemoveBackslashUnlessRoot(Comparable), Directory) <> 0 then begin
      if not First then Result := Result + ';';
      Result := Result + Segment;
      First := False;
    end;
  until Delimiter = 0;
end;

procedure LifecycleRemoveOwnedPath(const Directory: String);
var
  Value, Updated: String;
  Key: THandle;
  ValueType, Size: Cardinal;
begin
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', Value) then Exit;
  Updated := LifecycleRemovePathSegment(Value, Directory);
  if Value = Updated then Exit;
  if LifecycleOpenKey($80000001, 'Environment', 0, 1, Key) <> 0 then
    RaiseException('Cannot inspect the per-user PATH value.');
  try
    Size := 0;
    if LifecycleQueryValue(Key, 'Path', 0, ValueType, 0, Size) <> 0 then
      RaiseException('Cannot inspect the per-user PATH type.');
  finally
    LifecycleCloseKey(Key);
  end;
  if ValueType = 2 then begin
    if not RegWriteExpandStringValue(HKCU, 'Environment', 'Path', Updated) then
      RaiseException('Cannot remove the owned per-user PATH segment.');
  end else if ValueType = 1 then begin
    if not RegWriteStringValue(HKCU, 'Environment', 'Path', Updated) then
      RaiseException('Cannot remove the owned per-user PATH segment.');
  end else RaiseException('Unexpected per-user PATH type; leaving it unchanged.');
end;

function LifecycleOwnedLink(const Link, Target: String): Boolean;
begin
  Result := (Link <> '') and not LifecyclePathIsRelative(Link) and
    (LifecycleFileAttributes(Link) <> $FFFFFFFF) and
    ((LifecycleFileAttributes(Link) and $400) <> 0) and LifecycleSamePath(Link, Target);
end;

function InitializeSetup: Boolean;
begin
  LifecycleDiscoverLegacy;
  Result := True;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  try
    LifecycleTarget := AddBackslash(ExpandConstant('{app}')) + '{#AppExeName}';
    if (LifecycleLegacyTarget <> '') and
       (CompareText(LifecycleNormalizedPath(LifecycleTarget), LifecycleLegacyTarget) <> 0) then
      RaiseException('The portable installation must be upgraded in its existing directory.');
    if not LifecyclePrepared then begin
      LifecycleOldInnoExists := RegQueryStringValue(HKCU64, LifecycleOwnKey,
        'DisplayVersion', LifecycleOldInnoVersion);
      if FileExists(LifecycleTarget) then begin
        LifecycleOldHash := GetSHA256OfFile(LifecycleTarget);
        LifecycleBackup := ExpandConstant('{tmp}\previous-application.exe');
        if not FileCopy(LifecycleTarget, LifecycleBackup, False) then
          RaiseException('The existing executable could not be backed up.');
      end;
      if '{#AdditionalPayloadName}' <> '' then begin
        LifecycleExtraTarget := AddBackslash(ExpandConstant('{app}')) + '{#AdditionalPayloadName}';
        LifecycleExtraExisted := FileExists(LifecycleExtraTarget);
        if LifecycleExtraExisted then begin
          LifecycleExtraHash := GetSHA256OfFile(LifecycleExtraTarget);
          LifecycleExtraBackup := ExpandConstant('{tmp}\previous-additional-payload.exe');
          if not FileCopy(LifecycleExtraTarget, LifecycleExtraBackup, False) then
            RaiseException('The additional executable could not be backed up.');
        end;
      end;
      LifecycleCapture(LifecycleTarget, LifecycleHandles);
      LifecycleWasRunning := GetArrayLength(LifecycleHandles) > 0;
      LifecyclePrepared := True;
      if LifecycleWasRunning then LifecycleRetainLaunchToken(LifecycleHandles[0]);
    end;
    LifecycleStop(LifecycleTarget, LifecycleHandles);
    LifecycleReleaseHandles(LifecycleHandles);
  except
    Result := GetExceptionMessage;
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssDone then begin
    if LifecycleWasRunning then begin
      LifecycleRestarted := LifecycleStart(LifecycleTarget);
      if not LifecycleRestarted then RaiseException('The updated application did not restart.');
    end;
    { Keep the old ARP entry until the complete installation and requested
      relaunch succeed. The old WinGet alias still resolves to the same path. }
    if LifecycleOwnedLink(LifecycleLegacyLink, LifecycleTarget) then
      if not RegWriteStringValue(HKCU64, LifecycleOwnKey, 'AdoptedWinGetSymlink', LifecycleLegacyLink) then
        RaiseException('The migrated command alias could not be recorded.');
    if LifecycleLegacyPathAdded then
      if not RegWriteStringValue(HKCU64, LifecycleOwnKey, 'AdoptedWinGetPath', LifecycleLegacyDirectory) then
        RaiseException('The migrated PATH entry could not be recorded.');
    if LifecycleLegacyKey <> '' then
      if not RegDeleteKeyIncludingSubkeys(HKCU64, LifecycleLegacyKey) then
        RaiseException('The old portable registration could not be retired.');
    LifecycleCompleted := True;
    Log('Package installation completed; previous running state was preserved.');
  end;
end;

function GetCustomSetupExitCode: Integer;
begin
  if LifecycleCompleted then Result := 0 else Result := 1;
end;

procedure DeinitializeSetup;
var
  Handles: TLifecycleHandles;
  ExistingLocation: String;
begin
  try
    LifecycleReleaseHandles(LifecycleHandles);
    if not LifecyclePrepared or LifecycleCompleted then Exit;
  { Inno rolls its transaction back on installation failure. The private copy
    also covers failures during post-install relaunch, after file replacement. }
  if (LifecycleBackup <> '') and FileExists(LifecycleBackup) then begin
    if not FileExists(LifecycleTarget) or (GetSHA256OfFile(LifecycleTarget) <> LifecycleOldHash) then begin
      LifecycleCapture(LifecycleTarget, Handles);
      try
        LifecycleStop(LifecycleTarget, Handles);
        if not FileCopy(LifecycleBackup, LifecycleTarget, False) then
          RaiseException('The previous executable could not be restored after installation failure.');
      finally
        LifecycleReleaseHandles(Handles);
      end;
    end;
    if LifecycleOldInnoExists then
      RegWriteStringValue(HKCU64, LifecycleOwnKey, 'DisplayVersion', LifecycleOldInnoVersion)
    else if RegQueryStringValue(HKCU64, LifecycleOwnKey, 'InstallLocation', ExistingLocation) and
      (CompareText(LifecycleNormalizedPath(ExistingLocation), ExtractFileDir(LifecycleTarget)) = 0) then
      RegDeleteKeyIncludingSubkeys(HKCU64, LifecycleOwnKey);
  end;
  if LifecycleExtraTarget <> '' then begin
    if LifecycleExtraExisted then begin
      if not FileExists(LifecycleExtraTarget) or
        (GetSHA256OfFile(LifecycleExtraTarget) <> LifecycleExtraHash) then
        if not FileCopy(LifecycleExtraBackup, LifecycleExtraTarget, False) then
          RaiseException('The additional executable could not be restored.');
    end else if FileExists(LifecycleExtraTarget) then
      if not DeleteFile(LifecycleExtraTarget) then
        RaiseException('The new additional executable could not be rolled back.');
  end;
  if LifecycleWasRunning then begin
    LifecycleCapture(LifecycleTarget, Handles);
    try
      if GetArrayLength(Handles) = 0 then
        if not LifecycleStart(LifecycleTarget) then
          RaiseException('The previous application could not be restarted after installation failure.');
    finally
      LifecycleReleaseHandles(Handles);
    end;
  end;
  finally
    if LifecycleLaunchToken <> 0 then begin
      LifecycleCloseHandle(LifecycleLaunchToken);
      LifecycleLaunchToken := 0;
    end;
  end;
end;

function InitializeUninstall: Boolean;
begin
  Result := True;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Target, Link, Command, ExpectedCommand, Directory: String;
  Handles: TLifecycleHandles;
begin
  if CurUninstallStep <> usUninstall then Exit;
  Target := ExpandConstant('{app}\{#AppExeName}');
  LifecycleCapture(Target, Handles);
  try
    LifecycleStop(Target, Handles);
  finally
    LifecycleReleaseHandles(Handles);
  end;
  ExpectedCommand := '"' + Target + '"';
  if '{#AutostartArguments}' <> '' then ExpectedCommand := ExpectedCommand + ' {#AutostartArguments}';
  if RegQueryStringValue(HKCU64, LifecycleRunKey, '{#RunValueName}', Command) and
    (CompareText(Command, ExpectedCommand) = 0) then
    RegDeleteValue(HKCU64, LifecycleRunKey, '{#RunValueName}');
  if RegQueryStringValue(HKCU64, LifecycleOwnKey, 'AdoptedWinGetSymlink', Link) and
    LifecycleOwnedLink(Link, Target) then
    if not DeleteFile(Link) then RaiseException('The owned command alias could not be removed.');
  if RegQueryStringValue(HKCU64, LifecycleOwnKey, 'AdoptedWinGetPath', Directory) and
    (CompareText(LifecycleNormalizedPath(Directory), ExtractFileDir(Target)) = 0) then
    LifecycleRemoveOwnedPath(Directory);
end;
