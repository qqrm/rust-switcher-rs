#if (VER < EncodeVer(6, 0, 0)) || (VER >= EncodeVer(7, 0, 0))
  #error This native lifecycle requires the 32-bit Inno Setup 6 compiler
#endif
#pragma message "Compiling native lifecycle with Inno Setup " + DecodeVer(VER)

{ Shared per-user application lifecycle. Include after [Code].
  Required defines: AppName, AppExeName, PackageIdentifier, RunValueName,
  AppId, RestartArguments, AutostartArguments, DefaultInstallDirectory.
  CloseApplications and RestartApplications must both be disabled: legacy
  portable releases do not register with Windows Restart Manager. }
#ifndef AdditionalPayloadName
  #define AdditionalPayloadName ""
#endif
#ifndef QuitPidArgument
  #define QuitPidArgument ""
#endif

const
  LifecycleUninstallRoot = 'Software\Microsoft\Windows\CurrentVersion\Uninstall';
  LifecycleRunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';
  LifecycleQuery = $1000;
  LifecycleSynchronize = $100000;
  LifecycleTerminate = $0001;
  LifecycleWaitTimeout = 258;
  LifecycleOfficialSource = 'Microsoft.Winget.Source_8wekyb3d8bbwe';
  { ASSIGN_PRIMARY | DUPLICATE | QUERY | ADJUST_DEFAULT | ADJUST_SESSIONID.
    Matches the explicit duplicate-handle access used before token launch in
    Chromium 140.0.7339.50: base/win/elevation_util.cc. The native probe compares
    this mask with the documented 0xB launch mask. }
  LifecycleLaunchTokenAccess = $18B;

type
  TLifecycleProcessIds = array[0..8191] of Cardinal;
  TLifecycleHandles = array[0..8191] of THandle;
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
function LifecycleGetProcessId(Process: THandle): Cardinal;
  external 'GetProcessId@kernel32.dll stdcall';
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
  LifecycleHandleCount: Integer;

function LifecycleOwnKey: String;
begin
  Result := LifecycleUninstallRoot + '\{#AppId}_is1';
end;

function LifecycleNormalizedPath(const Path: String): String;
begin
  Result := RemoveBackslashUnlessRoot(ExpandFileName(Path));
end;

function LifecycleApiError(const Message: String): String;
var
  ErrorCode: Integer;
begin
  { Capture Inno's saved result of the last script DLL call before cleanup calls.
    A direct GetLastError call could observe Setup's own intervening API calls. }
  ErrorCode := DLLGetLastError;
  Result := Message + ' (Win32 ' + IntToStr(ErrorCode) + ': ' + SysErrorMessage(ErrorCode) + ')';
end;

function LifecycleWaitError(const Message: String; WaitResult: Cardinal): String;
begin
  if WaitResult = $FFFFFFFF then
    Result := LifecycleApiError(Message + ' [WaitForSingleObject]')
  else Result := Message + ' (wait result=' + IntToStr(WaitResult) + ')';
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
  Log('[lifecycle] Legacy discovery: begin');
  LifecycleDetected := True;
  if not RegGetSubkeyNames(HKCU64, LifecycleUninstallRoot, Names) then Exit;
  Log('[lifecycle] Legacy discovery: registry entries=' + IntToStr(GetArrayLength(Names)));
  for Index := 0 to GetArrayLength(Names) - 1 do begin
    Key := LifecycleUninstallRoot + '\' + Names[Index];
    if not RegQueryStringValue(HKCU64, Key, 'WinGetPackageIdentifier', PackageId) then Continue;
    if CompareText(PackageId, '{#PackageIdentifier}') <> 0 then Continue;
    if not RegQueryStringValue(HKCU64, Key, 'WinGetSourceIdentifier', SourceId) then Continue;
    if SourceId <> LifecycleOfficialSource then Continue;
    if CompareText(Names[Index], '{#PackageIdentifier}_' + SourceId) <> 0 then Continue;
    if not RegQueryStringValue(HKCU64, Key, 'WinGetInstallerType', InstallerType) then Continue;
    if InstallerType <> 'portable' then Continue;
    Log('[lifecycle] Legacy discovery: validating ' + Key);
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
  Log('[lifecycle] Legacy discovery: complete; target=' + LifecycleLegacyTarget);
end;

function GetDefaultInstallDirectory(Param: String): String;
begin
  LifecycleDiscoverLegacy;
  if LifecycleLegacyDirectory <> '' then Result := LifecycleLegacyDirectory
  else Result := ExpandConstant('{#DefaultInstallDirectory}');
  Log('[lifecycle] Default installation directory: ' + Result);
end;

{ These synchronous lifecycle operations share one installer-owned snapshot.
  Keep its fixed buffer and count here: no array descriptors cross script calls.
  A snapshot must be released before another capture or application restart. }
procedure LifecycleReleaseHandles;
var
  Handle: THandle;
begin
  while LifecycleHandleCount > 0 do begin
    LifecycleHandleCount := LifecycleHandleCount - 1;
    Handle := LifecycleHandles[LifecycleHandleCount];
    LifecycleHandles[LifecycleHandleCount] := 0;
    if Handle <> 0 then LifecycleCloseHandle(Handle);
  end;
end;

procedure LifecycleCapture(const Target: String);
var
  Ids: TLifecycleProcessIds;
  Needed, Session, CurrentSession, Size, WaitResult: Cardinal;
  Index: Integer;
  Handle: THandle;
  Buffer, CanonicalTarget: String;
begin
  Log('[lifecycle] Process capture: begin; target=' + Target);
  if LifecycleHandleCount <> 0 then
    RaiseException('The previous process snapshot has not been released.');
  CanonicalTarget := LifecycleCanonicalPath(Target);
  Log('[lifecycle] Process capture: canonical target=' + CanonicalTarget);
  if CanonicalTarget = '' then Exit;
  if not LifecycleProcessSession(LifecycleCurrentProcessId, CurrentSession) then
    RaiseException(LifecycleApiError('Cannot identify the current Windows session [ProcessIdToSessionId]'));
  Log('[lifecycle] Process capture: enumerating session=' + IntToStr(CurrentSession));
  if not LifecycleEnumProcesses(Ids, 32768, Needed) then
    RaiseException(LifecycleApiError('Cannot take a process snapshot [EnumProcesses]'));
  if Needed >= 32768 then
    RaiseException('Cannot take a complete process snapshot.');
  Log('[lifecycle] Process capture: enumerated processes=' + IntToStr(Needed div 4));
  try
    for Index := 0 to (Needed div 4) - 1 do begin
      if not LifecycleProcessSession(Ids[Index], Session) or (Session <> CurrentSession) then Continue;
      Handle := LifecycleOpenProcess(LifecycleQuery or LifecycleSynchronize or LifecycleTerminate,
        False, Ids[Index]);
      if Handle = 0 then Continue;
      try
        SetLength(Buffer, 32768);
        Size := 32768;
        if LifecycleQueryImage(Handle, 0, Buffer, Size) then begin
          SetLength(Buffer, Size);
          if CompareText(LifecycleCanonicalPath(Buffer), CanonicalTarget) = 0 then begin
            WaitResult := LifecycleWait(Handle, 0);
            if (WaitResult <> 0) and (WaitResult <> LifecycleWaitTimeout) then
              RaiseException(LifecycleWaitError('Cannot inspect the captured application process', WaitResult));
            if WaitResult = LifecycleWaitTimeout then begin
              if LifecycleHandleCount >= 8192 then
                RaiseException('The process snapshot exceeds its fixed capacity.');
              LifecycleHandles[LifecycleHandleCount] := Handle;
              LifecycleHandleCount := LifecycleHandleCount + 1;
              Handle := 0;
              Log('[lifecycle] Process capture: retained PID=' + IntToStr(Ids[Index]));
            end;
          end;
        end;
      finally
        if Handle <> 0 then LifecycleCloseHandle(Handle);
      end;
    end;
  except
    Log('[lifecycle] Process capture: failed; ' + GetExceptionMessage);
    LifecycleReleaseHandles;
    RaiseException(GetExceptionMessage);
  end;
  Log('[lifecycle] Process capture: complete; handles=' + IntToStr(LifecycleHandleCount));
end;

procedure LifecycleStop(const Target: String);
var
  Index, ResultCode: Integer;
  WaitResult, ProcessId: Cardinal;
  QuitArguments: String;
begin
  Log('[lifecycle] Process stop: begin; target=' + Target + '; handles=' + IntToStr(LifecycleHandleCount));
  if LifecycleHandleCount = 0 then Exit;
  { The command is the production graceful shutdown route, including old portable
    versions. Only captured exact-path/current-session handles can be forced. }
  for Index := 0 to LifecycleHandleCount - 1 do begin
    WaitResult := LifecycleWait(LifecycleHandles[Index], 0);
    if WaitResult = 0 then Continue;
    if WaitResult <> LifecycleWaitTimeout then
      RaiseException(LifecycleWaitError('Cannot inspect the captured application before shutdown', WaitResult));
    QuitArguments := '--quit-for-update';
    if '{#QuitPidArgument}' <> '' then begin
      ProcessId := LifecycleGetProcessId(LifecycleHandles[Index]);
      if ProcessId = 0 then
        RaiseException(LifecycleApiError('Cannot identify the captured application process [GetProcessId]'));
      { Old versions recognize the plain flag. New versions also read the
        scoped argument and refuse to close a user-launched replacement. }
      QuitArguments := QuitArguments + ' {#QuitPidArgument}' + IntToStr(ProcessId);
    end;
    Log('[lifecycle] Process stop: launching graceful helper; arguments=' + QuitArguments);
    if not Exec(Target, QuitArguments, ExtractFileDir(Target), SW_HIDE, ewNoWait, ResultCode) then
      Log('Graceful shutdown helper could not start: ' + SysErrorMessage(ResultCode));
    Log('[lifecycle] Process stop: helper launch returned; waiting up to 10000 ms');
    WaitResult := LifecycleWait(LifecycleHandles[Index], 10000);
    Log('[lifecycle] Process stop: graceful wait result=' + IntToStr(WaitResult));
    if WaitResult = LifecycleWaitTimeout then begin
      Log('Graceful shutdown timed out; stopping the captured application handle.');
      if not LifecycleKill(LifecycleHandles[Index], 0) then
        RaiseException(LifecycleApiError('The running application could not be stopped [TerminateProcess]'));
      Log('[lifecycle] Process stop: termination returned; waiting up to 5000 ms');
      WaitResult := LifecycleWait(LifecycleHandles[Index], 5000);
      if WaitResult <> 0 then
        RaiseException(LifecycleWaitError('The running application did not exit before the update deadline', WaitResult));
    end else if WaitResult <> 0 then
      RaiseException(LifecycleWaitError('Waiting for the captured application failed', WaitResult));
  end;
  Log('[lifecycle] Process stop: complete');
end;

function LifecycleSameLogon(Left, Right: THandle): Boolean;
var
  LeftStats, RightStats: TLifecycleTokenStatistics;
  Needed: Cardinal;
begin
  Log('[lifecycle] Token identity: reading both token statistics');
  if not LifecycleTokenStatistics(Left, 10, LeftStats, 56, Needed) then
    RaiseException(LifecycleApiError('Cannot inspect the first logon identity [GetTokenInformation/TokenStatistics]'));
  if not LifecycleTokenStatistics(Right, 10, RightStats, 56, Needed) then
    RaiseException(LifecycleApiError('Cannot inspect the second logon identity [GetTokenInformation/TokenStatistics]'));
  { AuthenticationId is the LUID at byte offset 8; no native SID pointer is
    retained across Pascal Script's DLL marshalling boundary. }
  Result := (LeftStats[2] = RightStats[2]) and (LeftStats[3] = RightStats[3]);
  Log('[lifecycle] Token identity: comparison complete');
end;

procedure LifecycleRetainLaunchToken(ApplicationProcess: THandle);
var
  OwnToken, ApplicationToken, LinkedToken: THandle;
  Elevated, Needed, Index, LinkedTokenValue: Cardinal;
  Privileges: TLifecycleTokenPrivileges;
  Impersonate: TLifecycleLuid;
  Allowed, SameUser: Boolean;
begin
  Log('[lifecycle] Token retention: opening installer token');
  if not LifecycleOpenToken(THandle(-1), 8, OwnToken) then
    RaiseException(LifecycleApiError('Cannot inspect the installer security token [OpenProcessToken]'));
  try
    Log('[lifecycle] Token retention: reading installer elevation');
    if not LifecycleTokenDword(OwnToken, 20, Elevated, 4, Needed) then
      RaiseException(LifecycleApiError('Cannot inspect installer elevation [GetTokenInformation/TokenElevation]'));
    Log('[lifecycle] Token retention: installer elevation=' + IntToStr(Elevated));
    if Elevated = 0 then Exit;
    Log('[lifecycle] Token retention: opening application token');
    if not LifecycleOpenToken(ApplicationProcess, $B, ApplicationToken) then
      RaiseException(LifecycleApiError('Cannot retain the running application security token [OpenProcessToken]'));
    try
      Log('[lifecycle] Token retention: comparing logon identity');
      SameUser := LifecycleSameLogon(OwnToken, ApplicationToken);
      { UAC split tokens can identify different linked logon sessions. Only
        Windows' own linked token is accepted, never an unrelated user's token. }
      if not SameUser then begin
        if LifecycleTokenDword(OwnToken, 19, LinkedTokenValue, 4, Needed) then begin
          LinkedToken := THandle(LinkedTokenValue);
          try SameUser := LifecycleSameLogon(LinkedToken, ApplicationToken);
          finally LifecycleCloseHandle(LinkedToken); end;
        end else Log(LifecycleApiError('[lifecycle] Cannot inspect linked logon [GetTokenInformation/TokenLinkedToken]'));
      end;
      if not SameUser then
        RaiseException('The running application belongs to another Windows logon.');
      Log('[lifecycle] Token retention: verifying impersonation privilege');
      if not LifecycleLookupPrivilege(0, 'SeImpersonatePrivilege', Impersonate) then
        RaiseException(LifecycleApiError('Cannot identify the impersonation privilege [LookupPrivilegeValueW]'));
      if not LifecycleTokenPrivileges(OwnToken, 3, Privileges, 1024, Needed) then
        RaiseException(LifecycleApiError('Cannot inspect installer privileges [GetTokenInformation/TokenPrivileges]'));
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
      Log('[lifecycle] Token retention: duplicating primary token');
      if not LifecycleDuplicateToken(ApplicationToken, LifecycleLaunchTokenAccess, 0, 2, 1, LifecycleLaunchToken) then
        RaiseException(LifecycleApiError('The running application primary token could not be duplicated [DuplicateTokenEx]'));
      Log('[lifecycle] Token retention: primary token duplicated');
    finally
      LifecycleCloseHandle(ApplicationToken);
    end;
  finally
    LifecycleCloseHandle(OwnToken);
    Log('[lifecycle] Token retention: installer token released');
  end;
end;

function LifecycleStartWithToken(const Target: String): Boolean;
var
  Environment, WaitResult: Cardinal;
  Startup: TLifecycleStartupInfo;
  Process: TLifecycleProcessInformation;
begin
  Result := False;
  Log('[lifecycle] Token restart: copying environment');
  Environment := LifecycleEnvironment;
  if Environment = 0 then begin
    Log(LifecycleApiError('[lifecycle] Cannot copy the restart environment [GetEnvironmentStringsW]'));
    Exit;
  end;
  try
    Startup.Size := 68;
    Startup.Reserved := 0; Startup.Desktop := 0; Startup.Title := 0;
    Startup.X := 0; Startup.Y := 0; Startup.XSize := 0; Startup.YSize := 0;
    Startup.XCount := 0; Startup.YCount := 0; Startup.FillAttribute := 0; Startup.Flags := 0;
    Startup.ShowWindow := 0; Startup.ReservedSize := 0; Startup.ReservedBytes := 0;
    Startup.StandardInput := 0; Startup.StandardOutput := 0; Startup.StandardError := 0;
    { The copied Unicode environment preserves APPDATA and the caller's other
      per-user settings while the retained token preserves the app's privileges. }
    Log('[lifecycle] Token restart: CreateProcessWithTokenW begin; target=' + Target);
    if not LifecycleCreateWithToken(LifecycleLaunchToken, 0, Target,
        '"' + Target + '" {#RestartArguments}', $400, Environment,
        ExtractFileDir(Target), Startup, Process) then begin
      Log(LifecycleApiError('[lifecycle] Token restart failed [CreateProcessWithTokenW]'));
      Exit;
    end;
    Log('[lifecycle] Token restart: process created; PID=' + IntToStr(Process.ProcessId));
    try
      WaitResult := LifecycleWait(Process.Process, 1000);
      if WaitResult = $FFFFFFFF then
        Log(LifecycleWaitError('[lifecycle] Cannot inspect the restart process', WaitResult));
      Result := WaitResult = LifecycleWaitTimeout;
      Log('[lifecycle] Token restart: initial 1000 ms wait complete');
    finally
      LifecycleCloseHandle(Process.Thread);
      LifecycleCloseHandle(Process.Process);
    end;
  finally
    LifecycleFreeEnvironment(Environment);
    Log('[lifecycle] Token restart: environment released');
  end;
end;

function LifecycleStart(const Target: String): Boolean;
var
  ResultCode, Attempt: Integer;
  WaitResult: Cardinal;
begin
  Result := False;
  Log('[lifecycle] Restart: begin; target=' + Target);
  if LifecycleHandleCount <> 0 then
    RaiseException('The process snapshot must be released before restarting.');
  if not FileExists(Target) then Exit;
  Log('[lifecycle] Restart: launching through retained token or original-user route');
  if LifecycleLaunchToken <> 0 then begin
    Result := LifecycleStartWithToken(Target);
    { A user may win a concurrent launch. The internal --skip-if-running
      process then exits. Both launch routes must establish exactly one live
      exact-path replacement below, regardless of the launcher child's state. }
  end else if not ExecAsOriginalUser(Target, '{#RestartArguments}', ExtractFileDir(Target),
      SW_SHOWNORMAL, ewNoWait, ResultCode) then begin
    Log('Restart failed: ' + SysErrorMessage(ResultCode));
    Exit;
  end;
  Log('[lifecycle] Restart: launch returned; waiting for exactly one installed process');
  Result := False;
  for Attempt := 1 to 100 do begin
    LifecycleCapture(Target);
    try
      if LifecycleHandleCount = 1 then begin
        WaitResult := LifecycleWait(LifecycleHandles[0], 1000);
        if WaitResult = $FFFFFFFF then
          Log(LifecycleWaitError('[lifecycle] Cannot inspect the installed process', WaitResult));
        Result := WaitResult = LifecycleWaitTimeout;
        Log('[lifecycle] Restart: installed-process stability wait complete');
        Exit;
      end;
    finally
      LifecycleReleaseHandles;
    end;
    Sleep(50);
  end;
  Log('[lifecycle] Restart: readiness deadline exhausted');
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
  Log('[lifecycle] InitializeSetup: begin');
  LifecycleDiscoverLegacy;
  Result := True;
  Log('[lifecycle] InitializeSetup: complete');
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  Log('[lifecycle] PrepareToInstall: begin');
  try
    LifecycleTarget := AddBackslash(ExpandConstant('{app}')) + '{#AppExeName}';
    Log('[lifecycle] PrepareToInstall: target=' + LifecycleTarget);
    if (LifecycleLegacyTarget <> '') and
       (CompareText(LifecycleNormalizedPath(LifecycleTarget), LifecycleLegacyTarget) <> 0) then
      RaiseException('The portable installation must be upgraded in its existing directory.');
    if not LifecyclePrepared then begin
      LifecycleOldInnoExists := RegQueryStringValue(HKCU64, LifecycleOwnKey,
        'DisplayVersion', LifecycleOldInnoVersion);
      if FileExists(LifecycleTarget) then begin
        Log('[lifecycle] PrepareToInstall: hashing existing application');
        LifecycleOldHash := GetSHA256OfFile(LifecycleTarget);
        LifecycleBackup := ExpandConstant('{tmp}\previous-application.exe');
        Log('[lifecycle] PrepareToInstall: copying application backup');
        if not FileCopy(LifecycleTarget, LifecycleBackup, False) then
          RaiseException('The existing executable could not be backed up.');
        Log('[lifecycle] PrepareToInstall: application backup complete');
      end;
      if '{#AdditionalPayloadName}' <> '' then begin
        LifecycleExtraTarget := AddBackslash(ExpandConstant('{app}')) + '{#AdditionalPayloadName}';
        LifecycleExtraExisted := FileExists(LifecycleExtraTarget);
        if LifecycleExtraExisted then begin
          Log('[lifecycle] PrepareToInstall: hashing additional payload');
          LifecycleExtraHash := GetSHA256OfFile(LifecycleExtraTarget);
          LifecycleExtraBackup := ExpandConstant('{tmp}\previous-additional-payload.exe');
          Log('[lifecycle] PrepareToInstall: copying additional payload backup');
          if not FileCopy(LifecycleExtraTarget, LifecycleExtraBackup, False) then
            RaiseException('The additional executable could not be backed up.');
          Log('[lifecycle] PrepareToInstall: additional payload backup complete');
        end;
      end;
      Log('[lifecycle] PrepareToInstall: capturing running application');
      LifecycleCapture(LifecycleTarget);
      Log('[lifecycle] PrepareToInstall: received handles=' + IntToStr(LifecycleHandleCount));
      LifecycleWasRunning := LifecycleHandleCount > 0;
      if LifecycleWasRunning then LifecycleRetainLaunchToken(LifecycleHandles[0]);
      LifecyclePrepared := True;
      Log('[lifecycle] PrepareToInstall: capture and token preparation complete');
    end;
    Log('[lifecycle] PrepareToInstall: stopping captured application');
    LifecycleStop(LifecycleTarget);
    LifecycleReleaseHandles;
    Log('[lifecycle] PrepareToInstall: complete');
  except
    Log('[lifecycle] PrepareToInstall: failed; ' + GetExceptionMessage);
    if not LifecyclePrepared then LifecycleReleaseHandles;
    Result := GetExceptionMessage;
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  Log('[lifecycle] CurStepChanged: step=' + IntToStr(Ord(CurStep)));
  if CurStep = ssDone then begin
    if LifecycleWasRunning then begin
      Log('[lifecycle] Installation complete: restarting previously running application');
      LifecycleRestarted := LifecycleStart(LifecycleTarget);
      if not LifecycleRestarted then RaiseException('The updated application did not restart.');
      Log('[lifecycle] Installation complete: restart verified');
    end;
    { Keep the old ARP entry until the complete installation and requested
      relaunch succeed. The old WinGet alias still resolves to the same path. }
    Log('[lifecycle] Installation complete: recording migrated alias and PATH');
    if LifecycleOwnedLink(LifecycleLegacyLink, LifecycleTarget) then
      if not RegWriteStringValue(HKCU64, LifecycleOwnKey, 'AdoptedWinGetSymlink', LifecycleLegacyLink) then
        RaiseException('The migrated command alias could not be recorded.');
    if LifecycleLegacyPathAdded then
      if not RegWriteStringValue(HKCU64, LifecycleOwnKey, 'AdoptedWinGetPath', LifecycleLegacyDirectory) then
        RaiseException('The migrated PATH entry could not be recorded.');
    Log('[lifecycle] Installation complete: retiring legacy registration');
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
  Log('[lifecycle] Setup exit code=' + IntToStr(Result));
end;

procedure DeinitializeSetup;
var
  ExistingLocation: String;
  NeedsStart: Boolean;
begin
  Log('[lifecycle] DeinitializeSetup: begin');
  try
    LifecycleReleaseHandles;
    if not LifecyclePrepared or LifecycleCompleted then Exit;
  Log('[lifecycle] Rollback: begin');
  { Inno rolls its transaction back on installation failure. The private copy
    also covers failures during post-install relaunch, after file replacement. }
  if (LifecycleBackup <> '') and FileExists(LifecycleBackup) then begin
    Log('[lifecycle] Rollback: checking application hash');
    if not FileExists(LifecycleTarget) or (GetSHA256OfFile(LifecycleTarget) <> LifecycleOldHash) then begin
      LifecycleCapture(LifecycleTarget);
      try
        LifecycleStop(LifecycleTarget);
        Log('[lifecycle] Rollback: restoring application backup');
        if not FileCopy(LifecycleBackup, LifecycleTarget, False) then
          RaiseException('The previous executable could not be restored after installation failure.');
        Log('[lifecycle] Rollback: application restored');
      finally
        LifecycleReleaseHandles;
      end;
    end;
    Log('[lifecycle] Rollback: restoring registration');
    if LifecycleOldInnoExists then
      RegWriteStringValue(HKCU64, LifecycleOwnKey, 'DisplayVersion', LifecycleOldInnoVersion)
    else if RegQueryStringValue(HKCU64, LifecycleOwnKey, 'InstallLocation', ExistingLocation) and
      (CompareText(LifecycleNormalizedPath(ExistingLocation), ExtractFileDir(LifecycleTarget)) = 0) then
      RegDeleteKeyIncludingSubkeys(HKCU64, LifecycleOwnKey);
  end;
  Log('[lifecycle] Rollback: restoring additional payload if needed');
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
  Log('[lifecycle] Rollback: restoring previous running state if needed');
  if LifecycleWasRunning then begin
    LifecycleCapture(LifecycleTarget);
    try
      NeedsStart := LifecycleHandleCount = 0;
    finally
      LifecycleReleaseHandles;
    end;
    if NeedsStart then
      if not LifecycleStart(LifecycleTarget) then
        RaiseException('The previous application could not be restarted after installation failure.');
  end;
  Log('[lifecycle] Rollback: complete');
  finally
    if LifecycleLaunchToken <> 0 then begin
      LifecycleCloseHandle(LifecycleLaunchToken);
      LifecycleLaunchToken := 0;
    end;
    Log('[lifecycle] DeinitializeSetup: complete');
  end;
end;

function InitializeUninstall: Boolean;
begin
  Log('[lifecycle] InitializeUninstall: begin');
  Result := True;
  Log('[lifecycle] InitializeUninstall: complete');
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Target, Link, Command, ExpectedCommand, Directory: String;
begin
  Log('[lifecycle] CurUninstallStepChanged: step=' + IntToStr(Ord(CurUninstallStep)));
  if CurUninstallStep <> usUninstall then Exit;
  Target := ExpandConstant('{app}\{#AppExeName}');
  Log('[lifecycle] Uninstall: capturing and stopping application; target=' + Target);
  LifecycleCapture(Target);
  try
    LifecycleStop(Target);
  finally
    LifecycleReleaseHandles;
  end;
  Log('[lifecycle] Uninstall: removing owned autostart entry and alias');
  ExpectedCommand := '"' + Target + '"';
  if '{#AutostartArguments}' <> '' then ExpectedCommand := ExpectedCommand + ' {#AutostartArguments}';
  if RegQueryStringValue(HKCU64, LifecycleRunKey, '{#RunValueName}', Command) and
    (CompareText(Command, ExpectedCommand) = 0) then
    RegDeleteValue(HKCU64, LifecycleRunKey, '{#RunValueName}');
  if RegQueryStringValue(HKCU64, LifecycleOwnKey, 'AdoptedWinGetSymlink', Link) and
    LifecycleOwnedLink(Link, Target) then
    if not DeleteFile(Link) then RaiseException('The owned command alias could not be removed.');
  Log('[lifecycle] Uninstall: removing owned PATH entry if needed');
  if RegQueryStringValue(HKCU64, LifecycleOwnKey, 'AdoptedWinGetPath', Directory) and
    (CompareText(LifecycleNormalizedPath(Directory), ExtractFileDir(Target)) = 0) then
    LifecycleRemoveOwnedPath(Directory);
  Log('[lifecycle] Uninstall: lifecycle complete');
end;
