#if (VER < EncodeVer(6, 0, 0)) || (VER >= EncodeVer(7, 0, 0))
  #error This probe requires the 32-bit Inno Setup 6 compiler
#endif
#pragma message "Compiling array probe with Inno Setup " + DecodeVer(VER)

[Setup]
AppId=QQRM.InnoArrayTransferProbe
AppName=Inno Array Transfer Probe
AppVersion=0.0.0
DefaultDirName={tmp}\InnoArrayTransferProbe
CreateAppDir=no
Uninstallable=no
CreateUninstallRegKey=no
PrivilegesRequired=lowest
CloseApplications=no
RestartApplications=no
SetupLogging=yes
OutputBaseFilename=inno-array-probe
DisableWelcomePage=yes
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableReadyPage=yes
DisableFinishedPage=yes

[Code]
{ No files, registration, process shutdown, or application changes. Dynamic
  array handoffs are observations, including failures. The global fixed owner
  and read-only native operations are required controls. Only the current
  process is opened, and every real handle is closed in the same scope. }
type
  TProbeValues = array of THandle;
  TProbeIds = array[0..8191] of Cardinal;
  TProbeFixedHandles = array[0..8191] of THandle;

function ProbeEnumProcesses(var Ids: TProbeIds; Bytes: Cardinal;
  var Needed: Cardinal): Boolean;
  external 'EnumProcesses@psapi.dll stdcall';
function ProbeCurrentProcessId: Cardinal;
  external 'GetCurrentProcessId@kernel32.dll stdcall';
function ProbeOpenProcess(Access: Cardinal; Inherit: Boolean; Id: Cardinal): THandle;
  external 'OpenProcess@kernel32.dll stdcall';
function ProbeCloseHandle(Handle: THandle): Boolean;
  external 'CloseHandle@kernel32.dll stdcall';
function ProbeGetProcessId(Handle: THandle): Cardinal;
  external 'GetProcessId@kernel32.dll stdcall';
function ProbeProcessSession(Id: Cardinal; var Session: Cardinal): Boolean;
  external 'ProcessIdToSessionId@kernel32.dll stdcall';
function ProbeQueryImage(Handle: THandle; Flags: Cardinal; Buffer: String;
  var Size: Cardinal): Boolean;
  external 'QueryFullProcessImageNameW@kernel32.dll stdcall';
function ProbeOpenFile(Name: String; Access, Share, Security, Creation,
  Flags: Cardinal; Template: THandle): THandle;
  external 'CreateFileW@kernel32.dll stdcall';
function ProbeFinalPath(Handle: THandle; Buffer: String; Size, Flags: Cardinal): Cardinal;
  external 'GetFinalPathNameByHandleW@kernel32.dll stdcall';
function ProbeWait(Handle: THandle; Milliseconds: Cardinal): Cardinal;
  external 'WaitForSingleObject@kernel32.dll stdcall';

var
  ProbeGlobal, ProbeSentinel: TProbeValues;
  ProbeFixed: TProbeFixedHandles;
  ProbeFixedCount, ProbeRequiredFailures, ProbeObservationCount: Integer;
  ProbeCompleted: Boolean;

procedure ProbeObserve(const Name: String; Actual, Expected: Integer; Required: Boolean);
begin
  ProbeObservationCount := ProbeObservationCount + 1;
  Log('[array-probe] name=' + Name + ';actual=' + IntToStr(Actual) +
    ';expected=' + IntToStr(Expected) + ';required=' + IntToStr(Ord(Required)));
  if Required and (Actual <> Expected) then
    ProbeRequiredFailures := ProbeRequiredFailures + 1;
end;

procedure ProbePureVar(var Values: TProbeValues);
begin
  SetArrayLength(Values, 1);
  Values[0] := 42;
  ProbeObserve('pure-var-inner-count', GetArrayLength(Values), 1, False);
end;

function ProbePureResult: TProbeValues;
begin
  SetArrayLength(Result, 1);
  Result[0] := 42;
  ProbeObserve('pure-result-inner-count', GetArrayLength(Result), 1, False);
end;

procedure ProbeNativeOperations(Stage: Integer; var Ids: TProbeIds);
var
  Needed, OwnId, Size, Session: Cardinal;
  Index: Integer;
  Seen: Boolean;
  ProcessHandle, FileHandle: THandle;
  Buffer, ImagePath: String;
begin
  Ids[0] := 17;
  Ids[8191] := 23;
  ProbeObserve('large-frame-first', Ids[0], 17, True);
  ProbeObserve('large-frame-last', Ids[8191], 23, True);
  if Stage = 0 then Exit;
  OwnId := ProbeCurrentProcessId;
  Needed := 0;
  if not ProbeEnumProcesses(Ids, 32768, Needed) or
      (Needed = 0) or (Needed >= 32768) or ((Needed mod 4) <> 0) then
    RaiseException('The native process snapshot was incomplete.');
  Seen := False;
  for Index := 0 to (Needed div 4) - 1 do
    if Ids[Index] = OwnId then Seen := True;
  ProbeObserve('enum-contains-self', Ord(Seen), 1, True);
  if Stage = 1 then Exit;
  if not ProbeProcessSession(OwnId, Session) then
    RaiseException('The current process session could not be queried.');
  ProcessHandle := ProbeOpenProcess($1000 or $100000, False, OwnId);
  if ProcessHandle = 0 then RaiseException('The current process could not be opened.');
  try
    ProbeObserve('opened-self-identity', Ord(ProbeGetProcessId(ProcessHandle) = OwnId), 1, True);
    ProbeObserve('opened-self-live', ProbeWait(ProcessHandle, 0), 258, True);
    if Stage = 2 then Exit;
    SetLength(Buffer, 32768);
    Size := 32768;
    if not ProbeQueryImage(ProcessHandle, 0, Buffer, Size) or
        (Size = 0) or (Size >= 32768) then
      RaiseException('The current process image buffer could not be read.');
    SetLength(Buffer, Size);
    ProbeObserve('image-buffer-length', Length(Buffer), Size, True);
    if Stage = 3 then Exit;
    ImagePath := Buffer;
    FileHandle := ProbeOpenFile(ImagePath, 0, 7, 0, 3, $02000000, 0);
    if FileHandle = THandle(-1) then RaiseException('The current image could not be opened.');
    try
      SetLength(Buffer, 32768);
      Size := ProbeFinalPath(FileHandle, Buffer, 32768, 0);
      if (Size = 0) or (Size >= 32768) then
        RaiseException('The canonical path buffer could not be read.');
      SetLength(Buffer, Size);
      ProbeObserve('canonical-buffer-length', Length(Buffer), Size, True);
    finally
      ProbeObserve('file-handle-closed', Ord(ProbeCloseHandle(FileHandle)), 1, True);
    end;
  finally
    ProbeObserve('process-handle-closed', Ord(ProbeCloseHandle(ProcessHandle)), 1, True);
  end;
end;

procedure ProbeLargeVar(Stage: Integer; var Values: TProbeValues);
var
  Ids: TProbeIds;
begin
  ProbeNativeOperations(Stage, Ids);
  SetArrayLength(Values, 1);
  Values[0] := 42;
  ProbeObserve('large-var-inner-' + IntToStr(Stage), GetArrayLength(Values), 1, False);
end;

function ProbeLargeResult(Stage: Integer): TProbeValues;
var
  Ids: TProbeIds;
begin
  ProbeNativeOperations(Stage, Ids);
  SetArrayLength(Result, 1);
  Result[0] := 42;
  ProbeObserve('large-result-inner-' + IntToStr(Stage), GetArrayLength(Result), 1, False);
end;

procedure ProbeDirectGlobal(Stage: Integer);
var
  Ids: TProbeIds;
begin
  ProbeNativeOperations(Stage, Ids);
  SetArrayLength(ProbeGlobal, 1);
  ProbeGlobal[0] := 42;
  ProbeObserve('direct-global-inner-' + IntToStr(Stage), GetArrayLength(ProbeGlobal), 1, False);
end;

procedure ProbeStaticCapture;
var
  Ids: TProbeIds;
  Handle: THandle;
begin
  if ProbeFixedCount <> 0 then RaiseException('The static owner was not empty.');
  ProbeNativeOperations(4, Ids);
  Handle := ProbeOpenProcess($1000 or $100000, False, ProbeCurrentProcessId);
  if Handle = 0 then RaiseException('The static owner could not open the current process.');
  try
    ProbeFixed[ProbeFixedCount] := Handle;
    ProbeFixedCount := ProbeFixedCount + 1;
    Handle := 0;
    ProbeObserve('static-inner-count', ProbeFixedCount, 1, True);
  finally
    if Handle <> 0 then ProbeCloseHandle(Handle);
  end;
end;

procedure ProbeStaticRelease;
var
  Handle: THandle;
begin
  while ProbeFixedCount > 0 do begin
    ProbeFixedCount := ProbeFixedCount - 1;
    Handle := ProbeFixed[ProbeFixedCount];
    ProbeFixed[ProbeFixedCount] := 0;
    ProbeObserve('static-handle-closed', Ord(ProbeCloseHandle(Handle)), 1, True);
  end;
end;

procedure ProbeRun;
var
  Local: TProbeValues;
  Stage: Integer;
begin
  SetArrayLength(ProbeSentinel, 1);
  ProbeSentinel[0] := 91;
  SetArrayLength(ProbeGlobal, 1);
  ProbeGlobal[0] := 42;
  ProbeObserve('direct-global-baseline-count', GetArrayLength(ProbeGlobal), 1, True);
  ProbeObserve('direct-global-baseline-value', ProbeGlobal[0], 42, True);
  SetArrayLength(ProbeGlobal, 0);
  ProbePureVar(ProbeGlobal);
  ProbeObserve('pure-var-global-count', GetArrayLength(ProbeGlobal), 1, False);
  if GetArrayLength(ProbeGlobal) > 0 then
    ProbeObserve('pure-var-global-value', ProbeGlobal[0], 42, False);
  ProbePureVar(Local);
  ProbeObserve('pure-var-local-count', GetArrayLength(Local), 1, False);
  if GetArrayLength(Local) > 0 then ProbeObserve('pure-var-local-value', Local[0], 42, False);
  SetArrayLength(ProbeGlobal, 0);
  ProbeGlobal := ProbePureResult;
  ProbeObserve('pure-result-global-count', GetArrayLength(ProbeGlobal), 1, False);
  if GetArrayLength(ProbeGlobal) > 0 then
    ProbeObserve('pure-result-global-value', ProbeGlobal[0], 42, False);
  SetArrayLength(Local, 0);
  Local := ProbePureResult;
  ProbeObserve('pure-result-local-count', GetArrayLength(Local), 1, False);
  if GetArrayLength(Local) > 0 then ProbeObserve('pure-result-local-value', Local[0], 42, False);

  { Stage 0 has a large local fixed buffer but no external DLL call. Later
    stages add enumeration, process handles, image output, and canonical path. }
  for Stage := 0 to 4 do begin
    Log('[array-probe] begin-stage=' + IntToStr(Stage));
    SetArrayLength(ProbeGlobal, 0);
    ProbeLargeVar(Stage, ProbeGlobal);
    ProbeObserve('large-var-global-count-' + IntToStr(Stage), GetArrayLength(ProbeGlobal), 1, False);
    if GetArrayLength(ProbeGlobal) > 0 then
      ProbeObserve('large-var-global-value-' + IntToStr(Stage), ProbeGlobal[0], 42, False);
    SetArrayLength(Local, 0);
    ProbeLargeVar(Stage, Local);
    ProbeObserve('large-var-local-count-' + IntToStr(Stage), GetArrayLength(Local), 1, False);
    SetArrayLength(ProbeGlobal, 0);
    ProbeGlobal := ProbeLargeResult(Stage);
    ProbeObserve('large-result-global-count-' + IntToStr(Stage), GetArrayLength(ProbeGlobal), 1, False);
    if GetArrayLength(ProbeGlobal) > 0 then
      ProbeObserve('large-result-global-value-' + IntToStr(Stage), ProbeGlobal[0], 42, False);
    SetArrayLength(Local, 0);
    Local := ProbeLargeResult(Stage);
    ProbeObserve('large-result-local-count-' + IntToStr(Stage), GetArrayLength(Local), 1, False);
    SetArrayLength(ProbeGlobal, 0);
    ProbeDirectGlobal(Stage);
    ProbeObserve('direct-global-count-' + IntToStr(Stage), GetArrayLength(ProbeGlobal), 1, False);
    if GetArrayLength(ProbeGlobal) > 0 then
      ProbeObserve('direct-global-value-' + IntToStr(Stage), ProbeGlobal[0], 42, False);
    ProbeObserve('sentinel-count-' + IntToStr(Stage), GetArrayLength(ProbeSentinel), 1, True);
    if GetArrayLength(ProbeSentinel) > 0 then
      ProbeObserve('sentinel-value-' + IntToStr(Stage), ProbeSentinel[0], 91, True);
  end;
  try
    ProbeStaticCapture;
    ProbeObserve('static-caller-count', ProbeFixedCount, 1, True);
    if ProbeFixedCount = 1 then begin
      ProbeObserve('static-caller-identity',
        Ord(ProbeGetProcessId(ProbeFixed[0]) = ProbeCurrentProcessId), 1, True);
      ProbeObserve('static-caller-live', ProbeWait(ProbeFixed[0], 0), 258, True);
    end;
  finally
    ProbeStaticRelease;
  end;
  ProbeObserve('static-released-count', ProbeFixedCount, 0, True);
  ProbeObserve('static-released-value', ProbeFixed[0], 0, True);
  ProbeCompleted := True;
end;

function InitializeSetup: Boolean;
begin
  Log('[array-probe] compiler={#DecodeVer(VER)}');
  try
    ProbeRun;
  except
    ProbeRequiredFailures := ProbeRequiredFailures + 1;
    Log('[array-probe] exception=' + GetExceptionMessage);
  end;
  Log('[array-probe] complete=' + IntToStr(Ord(ProbeCompleted)) +
    ';required-failures=' + IntToStr(ProbeRequiredFailures) +
    ';observations=' + IntToStr(ProbeObservationCount));
  Result := True;
end;

function GetCustomSetupExitCode: Integer;
begin
  if ProbeCompleted and (ProbeRequiredFailures = 0) then Result := 0
  else Result := 1;
end;

procedure DeinitializeSetup;
begin
  ProbeStaticRelease;
end;
