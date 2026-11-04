//! Lazy, verified model cache. Ranking never waits for network I/O.
//!
//! A single bounded worker handles missing models; requests are deduplicated by
//! language and failed requests have a retry cooldown. Only fixed, immutable
//! model URLs are contacted. Input text never enters this module.

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        mpsc::{self, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

use fst::Map;
use sha2::{Digest, Sha256};

use super::model_catalog::{MAX_MODEL_BYTES, MODELS, ModelSpec};

pub(super) type ProbabilityModel = Map<Cow<'static, [u8]>>;

const RETRY_DELAY: Duration = Duration::from_secs(5 * 60);

#[cfg(any(windows, test))]
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(45);
#[cfg(any(windows, test))]
const MIN_RETRY_BUDGET: Duration = Duration::from_secs(10);

#[cfg(any(windows, test))]
enum DownloadFailure {
    ResponseTimeout(io::Error),
    Other(io::Error),
}

#[cfg(any(windows, test))]
impl From<io::Error> for DownloadFailure {
    fn from(error: io::Error) -> Self {
        Self::Other(error)
    }
}

#[cfg(any(windows, test))]
impl DownloadFailure {
    fn into_io(self) -> io::Error {
        match self {
            Self::ResponseTimeout(error) | Self::Other(error) => error,
        }
    }
}

// A stalled response-header wait has been observed after a successful send.
// Retry that one failure with fresh handles, never a status or integrity error.
// Both attempts share their original time budget, checked at API boundaries.
// Synchronous WinHTTP phase timeouts do not provide hard wall-clock cancellation.
#[cfg(any(windows, test))]
fn retry_response_timeout<T>(
    started: Instant,
    mut attempt: impl FnMut() -> Result<T, DownloadFailure>,
) -> io::Result<T> {
    match attempt() {
        Ok(value) => Ok(value),
        Err(DownloadFailure::ResponseTimeout(error))
            if DOWNLOAD_TIMEOUT.saturating_sub(started.elapsed()) >= MIN_RETRY_BUDGET =>
        {
            tracing::debug!(error = %error, "retrying model response with a fresh request");
            #[cfg(test)]
            let _ = writeln!(
                io::stderr(),
                "WinHTTP model download: retrying once after {error}"
            );
            attempt().map_err(DownloadFailure::into_io)
        }
        Err(error) => Err(error.into_io()),
    }
}

#[cfg(any(windows, test))]
fn download_timeouts(started: Instant) -> io::Result<[i32; 4]> {
    let remaining_ms = DOWNLOAD_TIMEOUT
        .saturating_sub(started.elapsed())
        .as_millis();
    // WinHTTP treats zero as infinite. Reserve at least one millisecond for
    // each of the resolve/connect/send phases within WinHttpSendRequest.
    if remaining_ms < 3 {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "model download exhausted its shared 45-second budget",
        ));
    }
    let short = (remaining_ms / 4).clamp(1, 5_000) as i32;
    let send = (remaining_ms - 2 * short as u128).min(10_000) as i32;
    let receive = remaining_ms.min(10_000) as i32;
    Ok([short, short, send, receive])
}

#[derive(Default)]
struct CacheState {
    ready: HashMap<&'static str, Arc<ProbabilityModel>>,
    pending: HashSet<&'static str>,
    failed: HashMap<&'static str, Instant>,
}

impl CacheState {
    fn request(&mut self, code: &'static str, now: Instant) -> bool {
        if self.ready.contains_key(code)
            || self.pending.contains(code)
            || self
                .failed
                .get(code)
                .is_some_and(|failed| now.duration_since(*failed) < RETRY_DELAY)
        {
            return false;
        }
        self.pending.insert(code);
        true
    }
}

fn state() -> &'static Mutex<CacheState> {
    static STATE: OnceLock<Mutex<CacheState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(CacheState::default()))
}

pub(super) fn model(spec: &'static ModelSpec) -> Option<Arc<ProbabilityModel>> {
    {
        let mut state = state()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(model) = state.ready.get(spec.code) {
            return Some(Arc::clone(model));
        }
        if let Some(bytes) = embedded_bytes(spec) {
            let model = Arc::new(Map::new(Cow::Borrowed(bytes)).ok()?);
            state.ready.insert(spec.code, Arc::clone(&model));
            return Some(model);
        }
        if !state.request(spec.code, Instant::now()) {
            return None;
        }
    }

    if downloader().is_none_or(|sender| sender.try_send(spec).is_err()) {
        let mut state = state()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.pending.remove(spec.code);
        state.failed.insert(spec.code, Instant::now());
    }
    None
}

fn downloader() -> Option<&'static SyncSender<&'static ModelSpec>> {
    static SENDER: OnceLock<Option<SyncSender<&'static ModelSpec>>> = OnceLock::new();
    SENDER
        .get_or_init(|| {
            let (sender, receiver) = mpsc::sync_channel::<&'static ModelSpec>(MODELS.len());
            let spawned = thread::Builder::new()
                .name("layout-models".to_owned())
                .spawn(move || {
                    while let Ok(spec) = receiver.recv() {
                        let result = load_external_model(spec);
                        let mut state = state()
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        state.pending.remove(spec.code);
                        match result {
                            Ok(model) => {
                                state.ready.insert(spec.code, Arc::new(model));
                                state.failed.remove(spec.code);
                                tracing::debug!(language = spec.code, "layout model is ready");
                            }
                            Err(error) => {
                                state.failed.insert(spec.code, Instant::now());
                                tracing::debug!(
                                    language = spec.code,
                                    error = %error,
                                    "layout model unavailable; retaining deterministic fallback"
                                );
                            }
                        }
                    }
                });
            spawned.ok().map(|_| sender)
        })
        .as_ref()
}

fn cache_file(spec: &ModelSpec) -> io::Result<PathBuf> {
    let root = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA is unavailable"))?;
    Ok(root
        .join("RustSwitcher")
        .join("models")
        .join(spec.sha256)
        .join(format!("{}.fst", spec.code)))
}

fn load_external_model(spec: &ModelSpec) -> io::Result<ProbabilityModel> {
    load_cached_or_download(&cache_file(spec)?, spec, || download_model(spec))
}

fn load_cached_or_download(
    path: &Path,
    spec: &ModelSpec,
    download: impl FnOnce() -> io::Result<Vec<u8>>,
) -> io::Result<ProbabilityModel> {
    if let Ok(bytes) = read_verified(path, spec)
        && let Ok(model) = Map::new(Cow::Owned(bytes))
    {
        return Ok(model);
    }

    let bytes = download()?;
    verify_payload(spec, &bytes)?;
    let model = Map::new(Cow::Owned(bytes))
        .map_err(|_| invalid_data("model is not a valid FST probability map"))?;
    store_verified(path, spec, model.as_fst().as_bytes())?;
    Ok(model)
}

fn read_verified(path: &Path, spec: &ModelSpec) -> io::Result<Vec<u8>> {
    validate_size(spec)?;
    let file = File::open(path)?;
    if file.metadata()?.len() != spec.byte_len as u64 {
        return Err(invalid_data(
            "cached model length does not match the manifest",
        ));
    }
    let mut bytes = Vec::with_capacity(spec.byte_len);
    file.take(spec.byte_len as u64 + 1)
        .read_to_end(&mut bytes)?;
    verify_payload(spec, &bytes)?;
    Ok(bytes)
}

fn store_verified(path: &Path, spec: &ModelSpec, bytes: &[u8]) -> io::Result<()> {
    verify_payload(spec, bytes)?;
    let parent = path
        .parent()
        .ok_or_else(|| invalid_data("model cache path has no parent"))?;
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.flush()?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn validate_size(spec: &ModelSpec) -> io::Result<()> {
    if spec.byte_len == 0 || spec.byte_len > MAX_MODEL_BYTES {
        return Err(invalid_data("model length exceeds the configured limit"));
    }
    Ok(())
}

fn verify_payload(spec: &ModelSpec, bytes: &[u8]) -> io::Result<()> {
    validate_size(spec)?;
    if bytes.len() != spec.byte_len {
        return Err(invalid_data("model length does not match the manifest"));
    }
    if format!("{:x}", Sha256::digest(bytes)) != spec.sha256 {
        return Err(invalid_data("model SHA-256 does not match the manifest"));
    }
    Ok(())
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(not(windows))]
fn download_model(_spec: &ModelSpec) -> io::Result<Vec<u8>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "model downloads require Windows",
    ))
}

#[cfg(windows)]
fn download_model(spec: &ModelSpec) -> io::Result<Vec<u8>> {
    validate_size(spec)?;
    let started = Instant::now();
    retry_response_timeout(started, || download_model_attempt(spec, started))
}

#[cfg(windows)]
fn download_model_attempt(spec: &ModelSpec, started: Instant) -> Result<Vec<u8>, DownloadFailure> {
    use std::{ffi::c_void, ptr};

    use windows::{
        Win32::Networking::WinHttp::{
            ERROR_WINHTTP_TIMEOUT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
            WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2, WINHTTP_OPTION_REDIRECT_POLICY,
            WINHTTP_OPTION_REDIRECT_POLICY_NEVER, WINHTTP_OPTION_SECURE_PROTOCOLS,
            WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle,
            WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData,
            WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetOption, WinHttpSetTimeouts,
        },
        core::{PCWSTR, w},
    };

    use super::model_catalog::{MODEL_SOURCE_HOST, source_path};

    struct Handle(*mut c_void);

    impl Handle {
        fn new(value: *mut c_void) -> windows::core::Result<Self> {
            if value.is_null() {
                Err(windows::core::Error::from_thread())
            } else {
                Ok(Self(value))
            }
        }
    }

    impl Drop for Handle {
        fn drop(&mut self) {
            let _ = unsafe { WinHttpCloseHandle(self.0) };
        }
    }

    let windows_error = |operation: &str, error: windows::core::Error| {
        let kind = if error.code() == windows::core::HRESULT::from_win32(ERROR_WINHTTP_TIMEOUT) {
            io::ErrorKind::TimedOut
        } else {
            io::ErrorKind::Other
        };
        io::Error::new(
            kind,
            format!("{operation} after {:?}: {error}", started.elapsed()),
        )
    };
    let set_timeouts = |handle: *mut c_void| -> io::Result<()> {
        let [resolve, connect, send, receive] = download_timeouts(started)?;
        unsafe { WinHttpSetTimeouts(handle, resolve, connect, send, receive) }
            .map_err(|error| windows_error("WinHttpSetTimeouts", error))
    };
    let agent: Vec<u16> = concat!("RustSwitcher/", env!("CARGO_PKG_VERSION"))
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let host: Vec<u16> = MODEL_SOURCE_HOST.encode_utf16().chain(Some(0)).collect();
    let path: Vec<u16> = source_path(spec).encode_utf16().chain(Some(0)).collect();

    let session = Handle::new(unsafe {
        WinHttpOpen(
            PCWSTR(agent.as_ptr()),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        )
    })
    .map_err(|error| windows_error("WinHttpOpen", error))?;
    #[cfg(test)]
    install_network_diagnostics(session.0)
        .map_err(|error| windows_error("WinHttpSetStatusCallback", error))?;
    set_timeouts(session.0)?;
    let protocols = WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2.to_ne_bytes();
    unsafe {
        WinHttpSetOption(
            Some(session.0.cast_const()),
            WINHTTP_OPTION_SECURE_PROTOCOLS,
            Some(&protocols),
        )
    }
    .map_err(|error| windows_error("WinHttpSetOption(TLS)", error))?;

    let connection =
        Handle::new(unsafe { WinHttpConnect(session.0, PCWSTR(host.as_ptr()), 443, 0) })
            .map_err(|error| windows_error("WinHttpConnect", error))?;
    let request = Handle::new(unsafe {
        WinHttpOpenRequest(
            connection.0,
            w!("GET"),
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            ptr::null(),
            WINHTTP_FLAG_SECURE,
        )
    })
    .map_err(|error| windows_error("WinHttpOpenRequest", error))?;
    let redirect_policy = WINHTTP_OPTION_REDIRECT_POLICY_NEVER.to_ne_bytes();
    unsafe {
        WinHttpSetOption(
            Some(request.0.cast_const()),
            WINHTTP_OPTION_REDIRECT_POLICY,
            Some(&redirect_policy),
        )
    }
    .map_err(|error| windows_error("WinHttpSetOption(redirect policy)", error))?;
    set_timeouts(request.0)?;
    unsafe { WinHttpSendRequest(request.0, None, None, 0, 0, 0) }
        .map_err(|error| windows_error("WinHttpSendRequest", error))?;
    set_timeouts(request.0)?;
    unsafe { WinHttpReceiveResponse(request.0, ptr::null_mut()) }.map_err(|error| {
        let error = windows_error("WinHttpReceiveResponse", error);
        if error.kind() == io::ErrorKind::TimedOut {
            DownloadFailure::ResponseTimeout(error)
        } else {
            DownloadFailure::Other(error)
        }
    })?;
    let mut status = 0u32;
    let mut status_len = size_of::<u32>() as u32;
    unsafe {
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(ptr::from_mut(&mut status).cast()),
            &mut status_len,
            ptr::null_mut(),
        )
    }
    .map_err(|error| windows_error("WinHttpQueryHeaders(status)", error))?;
    if status != 200 {
        return Err(io::Error::other(format!("model server returned HTTP {status}")).into());
    }

    let mut bytes = Vec::with_capacity(spec.byte_len);
    loop {
        set_timeouts(request.0)?;
        let mut buffer = [0u8; 8192];
        let mut received = 0u32;
        unsafe {
            WinHttpReadData(
                request.0,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut received,
            )
        }
        .map_err(|error| windows_error("WinHttpReadData", error))?;
        if received == 0 {
            break;
        }
        let received = received as usize;
        if received > buffer.len() || received > spec.byte_len.saturating_sub(bytes.len()) {
            return Err(invalid_data("model response exceeds the manifest length").into());
        }
        bytes.extend_from_slice(&buffer[..received]);
    }
    download_timeouts(started)?;
    verify_payload(spec, &bytes)?;
    Ok(bytes)
}

// Hosted native tests need the phase of a transport failure, without logging
// proxy addresses, request headers, response bodies, or any user text. No
// callback or stderr output is installed in production builds.
#[cfg(all(windows, test))]
fn install_network_diagnostics(handle: *mut std::ffi::c_void) -> windows::core::Result<()> {
    use std::sync::atomic::{AtomicU32, Ordering};

    use windows::Win32::Networking::WinHttp::{
        WINHTTP_CALLBACK_FLAG_ALL_NOTIFICATIONS, WINHTTP_CALLBACK_STATUS_CONNECTED_TO_SERVER,
        WINHTTP_CALLBACK_STATUS_CONNECTING_TO_SERVER, WINHTTP_CALLBACK_STATUS_DETECTING_PROXY,
        WINHTTP_CALLBACK_STATUS_NAME_RESOLVED, WINHTTP_CALLBACK_STATUS_RECEIVING_RESPONSE,
        WINHTTP_CALLBACK_STATUS_REQUEST_SENT, WINHTTP_CALLBACK_STATUS_RESOLVING_NAME,
        WINHTTP_CALLBACK_STATUS_RESPONSE_RECEIVED, WINHTTP_CALLBACK_STATUS_SECURE_FAILURE,
        WINHTTP_CALLBACK_STATUS_SENDING_REQUEST, WinHttpSetStatusCallback,
    };

    // A response can trigger these notifications for every small body read.
    // The hosted test only needs the first observation of each phase per attempt.
    static SEEN_PHASES: AtomicU32 = AtomicU32::new(0);

    unsafe extern "system" fn callback(
        _handle: *mut std::ffi::c_void,
        _context: usize,
        status: u32,
        _information: *mut std::ffi::c_void,
        _information_len: u32,
    ) {
        let phase = match status {
            WINHTTP_CALLBACK_STATUS_DETECTING_PROXY => "detecting proxy",
            WINHTTP_CALLBACK_STATUS_RESOLVING_NAME => "resolving name",
            WINHTTP_CALLBACK_STATUS_NAME_RESOLVED => "name resolved",
            WINHTTP_CALLBACK_STATUS_CONNECTING_TO_SERVER => "connecting to server",
            WINHTTP_CALLBACK_STATUS_CONNECTED_TO_SERVER => "connected to server",
            WINHTTP_CALLBACK_STATUS_SENDING_REQUEST => "sending request",
            WINHTTP_CALLBACK_STATUS_REQUEST_SENT => "request sent",
            WINHTTP_CALLBACK_STATUS_RECEIVING_RESPONSE => "receiving response",
            WINHTTP_CALLBACK_STATUS_RESPONSE_RECEIVED => "response received",
            WINHTTP_CALLBACK_STATUS_SECURE_FAILURE => "secure connection failed",
            _ => return,
        };
        if SEEN_PHASES.fetch_or(status, Ordering::Relaxed) & status != 0 {
            return;
        }
        // Writing must not panic across the FFI callback boundary.
        let _ = writeln!(io::stderr(), "WinHTTP model download: {phase}");
    }

    SEEN_PHASES.store(0, Ordering::Relaxed);
    let previous = unsafe {
        WinHttpSetStatusCallback(
            handle,
            Some(callback),
            WINHTTP_CALLBACK_FLAG_ALL_NOTIFICATIONS,
            0,
        )
    };
    // WINHTTP_INVALID_STATUS_CALLBACK is the pointer-sized -1 sentinel.
    if previous.is_some_and(|callback| callback as usize == usize::MAX) {
        Err(windows::core::Error::from_thread())
    } else {
        Ok(())
    }
}

#[cfg(not(test))]
fn embedded_bytes(spec: &ModelSpec) -> Option<&'static [u8]> {
    let directory = match spec.code {
        "en" => lingua_english_language_model::ENGLISH_MODELS_DIRECTORY,
        "ru" => lingua_russian_language_model::RUSSIAN_MODELS_DIRECTORY,
        _ => return None,
    };
    directory.get_file("ngrams.fst").map(|file| file.contents())
}

#[cfg(test)]
fn embedded_bytes(spec: &ModelSpec) -> Option<&'static [u8]> {
    let directory = match spec.code {
        "af" => lingua_afrikaans_language_model::AFRIKAANS_MODELS_DIRECTORY,
        "sq" => lingua_albanian_language_model::ALBANIAN_MODELS_DIRECTORY,
        "ar" => lingua_arabic_language_model::ARABIC_MODELS_DIRECTORY,
        "hy" => lingua_armenian_language_model::ARMENIAN_MODELS_DIRECTORY,
        "az" => lingua_azerbaijani_language_model::AZERBAIJANI_MODELS_DIRECTORY,
        "eu" => lingua_basque_language_model::BASQUE_MODELS_DIRECTORY,
        "be" => lingua_belarusian_language_model::BELARUSIAN_MODELS_DIRECTORY,
        "bn" => lingua_bengali_language_model::BENGALI_MODELS_DIRECTORY,
        "nb" => lingua_bokmal_language_model::BOKMAL_MODELS_DIRECTORY,
        "bs" => lingua_bosnian_language_model::BOSNIAN_MODELS_DIRECTORY,
        "bg" => lingua_bulgarian_language_model::BULGARIAN_MODELS_DIRECTORY,
        "ca" => lingua_catalan_language_model::CATALAN_MODELS_DIRECTORY,
        "zh" => lingua_chinese_language_model::CHINESE_MODELS_DIRECTORY,
        "hr" => lingua_croatian_language_model::CROATIAN_MODELS_DIRECTORY,
        "cs" => lingua_czech_language_model::CZECH_MODELS_DIRECTORY,
        "da" => lingua_danish_language_model::DANISH_MODELS_DIRECTORY,
        "nl" => lingua_dutch_language_model::DUTCH_MODELS_DIRECTORY,
        "en" => lingua_english_language_model::ENGLISH_MODELS_DIRECTORY,
        "eo" => lingua_esperanto_language_model::ESPERANTO_MODELS_DIRECTORY,
        "et" => lingua_estonian_language_model::ESTONIAN_MODELS_DIRECTORY,
        "fi" => lingua_finnish_language_model::FINNISH_MODELS_DIRECTORY,
        "fr" => lingua_french_language_model::FRENCH_MODELS_DIRECTORY,
        "lg" => lingua_ganda_language_model::GANDA_MODELS_DIRECTORY,
        "ka" => lingua_georgian_language_model::GEORGIAN_MODELS_DIRECTORY,
        "de" => lingua_german_language_model::GERMAN_MODELS_DIRECTORY,
        "el" => lingua_greek_language_model::GREEK_MODELS_DIRECTORY,
        "gu" => lingua_gujarati_language_model::GUJARATI_MODELS_DIRECTORY,
        "he" => lingua_hebrew_language_model::HEBREW_MODELS_DIRECTORY,
        "hi" => lingua_hindi_language_model::HINDI_MODELS_DIRECTORY,
        "hu" => lingua_hungarian_language_model::HUNGARIAN_MODELS_DIRECTORY,
        "is" => lingua_icelandic_language_model::ICELANDIC_MODELS_DIRECTORY,
        "id" => lingua_indonesian_language_model::INDONESIAN_MODELS_DIRECTORY,
        "ga" => lingua_irish_language_model::IRISH_MODELS_DIRECTORY,
        "it" => lingua_italian_language_model::ITALIAN_MODELS_DIRECTORY,
        "ja" => lingua_japanese_language_model::JAPANESE_MODELS_DIRECTORY,
        "kk" => lingua_kazakh_language_model::KAZAKH_MODELS_DIRECTORY,
        "ko" => lingua_korean_language_model::KOREAN_MODELS_DIRECTORY,
        "la" => lingua_latin_language_model::LATIN_MODELS_DIRECTORY,
        "lv" => lingua_latvian_language_model::LATVIAN_MODELS_DIRECTORY,
        "lt" => lingua_lithuanian_language_model::LITHUANIAN_MODELS_DIRECTORY,
        "mk" => lingua_macedonian_language_model::MACEDONIAN_MODELS_DIRECTORY,
        "ms" => lingua_malay_language_model::MALAY_MODELS_DIRECTORY,
        "mi" => lingua_maori_language_model::MAORI_MODELS_DIRECTORY,
        "mr" => lingua_marathi_language_model::MARATHI_MODELS_DIRECTORY,
        "mn" => lingua_mongolian_language_model::MONGOLIAN_MODELS_DIRECTORY,
        "nn" => lingua_nynorsk_language_model::NYNORSK_MODELS_DIRECTORY,
        "fa" => lingua_persian_language_model::PERSIAN_MODELS_DIRECTORY,
        "pl" => lingua_polish_language_model::POLISH_MODELS_DIRECTORY,
        "pt" => lingua_portuguese_language_model::PORTUGUESE_MODELS_DIRECTORY,
        "pa" => lingua_punjabi_language_model::PUNJABI_MODELS_DIRECTORY,
        "ro" => lingua_romanian_language_model::ROMANIAN_MODELS_DIRECTORY,
        "ru" => lingua_russian_language_model::RUSSIAN_MODELS_DIRECTORY,
        "sr" => lingua_serbian_language_model::SERBIAN_MODELS_DIRECTORY,
        "sn" => lingua_shona_language_model::SHONA_MODELS_DIRECTORY,
        "sk" => lingua_slovak_language_model::SLOVAK_MODELS_DIRECTORY,
        "sl" => lingua_slovene_language_model::SLOVENE_MODELS_DIRECTORY,
        "so" => lingua_somali_language_model::SOMALI_MODELS_DIRECTORY,
        "st" => lingua_sotho_language_model::SOTHO_MODELS_DIRECTORY,
        "es" => lingua_spanish_language_model::SPANISH_MODELS_DIRECTORY,
        "sw" => lingua_swahili_language_model::SWAHILI_MODELS_DIRECTORY,
        "sv" => lingua_swedish_language_model::SWEDISH_MODELS_DIRECTORY,
        "tl" => lingua_tagalog_language_model::TAGALOG_MODELS_DIRECTORY,
        "ta" => lingua_tamil_language_model::TAMIL_MODELS_DIRECTORY,
        "te" => lingua_telugu_language_model::TELUGU_MODELS_DIRECTORY,
        "th" => lingua_thai_language_model::THAI_MODELS_DIRECTORY,
        "ts" => lingua_tsonga_language_model::TSONGA_MODELS_DIRECTORY,
        "tn" => lingua_tswana_language_model::TSWANA_MODELS_DIRECTORY,
        "tr" => lingua_turkish_language_model::TURKISH_MODELS_DIRECTORY,
        "uk" => lingua_ukrainian_language_model::UKRAINIAN_MODELS_DIRECTORY,
        "ur" => lingua_urdu_language_model::URDU_MODELS_DIRECTORY,
        "vi" => lingua_vietnamese_language_model::VIETNAMESE_MODELS_DIRECTORY,
        "cy" => lingua_welsh_language_model::WELSH_MODELS_DIRECTORY,
        "xh" => lingua_xhosa_language_model::XHOSA_MODELS_DIRECTORY,
        "yo" => lingua_yoruba_language_model::YORUBA_MODELS_DIRECTORY,
        "zu" => lingua_zulu_language_model::ZULU_MODELS_DIRECTORY,
        _ => return None,
    };
    directory.get_file("ngrams.fst").map(|file| file.contents())
}

#[cfg(test)]
mod tests {
    use super::{
        super::model_catalog::{
            MODEL_SOURCE_COMMIT, MODEL_SOURCE_HOST, language_for_tag, source_path,
        },
        *,
    };

    fn english() -> &'static ModelSpec {
        match language_for_tag("en-US") {
            Some(spec) => spec,
            None => panic!("English manifest missing"),
        }
    }

    fn english_bytes() -> &'static [u8] {
        match embedded_bytes(english()) {
            Some(bytes) => bytes,
            None => panic!("English test fixture missing"),
        }
    }

    #[test]
    fn every_manifest_matches_its_published_model_crate() {
        assert_eq!(MODELS.len(), 75);
        assert_eq!(MODEL_SOURCE_HOST, "raw.githubusercontent.com");
        assert_eq!(MODEL_SOURCE_COMMIT.len(), 40);
        assert!(
            MODEL_SOURCE_COMMIT
                .bytes()
                .all(|character| character.is_ascii_hexdigit())
        );
        let mut codes = HashSet::new();
        for spec in MODELS {
            assert!(codes.insert(spec.code), "duplicate {}", spec.code);
            assert_eq!(spec.code.len(), 2);
            assert!(
                spec.code
                    .bytes()
                    .all(|character| character.is_ascii_lowercase())
            );
            assert_eq!(spec.iso_639_3.len(), 3);
            assert_eq!(spec.sha256.len(), 64);
            assert!(
                spec.sha256
                    .bytes()
                    .all(|character| character.is_ascii_hexdigit())
            );
            assert!(source_path(spec).starts_with("/pemistahl/lingua-rs/"));
            let Some(bytes) = embedded_bytes(spec) else {
                panic!("Model fixture missing: {}", spec.code);
            };
            assert!(verify_payload(spec, bytes).is_ok(), "{}", spec.code);
            assert!(Map::new(bytes).is_ok(), "{}", spec.code);
        }
    }

    #[test]
    fn verified_cached_model_is_used_without_a_download() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("en.fst");
        store_verified(&path, english(), english_bytes())?;
        let loaded = load_cached_or_download(&path, english(), || {
            panic!("A valid cached model must not trigger a download");
        })?;
        assert!(loaded.contains_key("hello"));
        Ok(())
    }

    #[test]
    fn response_header_timeout_can_recover_with_one_fresh_attempt() -> io::Result<()> {
        let mut attempts = 0;
        let bytes = retry_response_timeout(Instant::now(), || {
            attempts += 1;
            if attempts == 1 {
                Err(DownloadFailure::ResponseTimeout(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "WinHttpReceiveResponse timed out",
                )))
            } else {
                Ok(vec![1, 2, 3])
            }
        })?;
        assert_eq!(attempts, 2);
        assert_eq!(bytes, [1, 2, 3]);
        Ok(())
    }

    #[test]
    fn repeated_response_header_timeouts_stop_after_two_attempts() {
        let mut attempts = 0;
        let result: io::Result<()> = retry_response_timeout(Instant::now(), || {
            attempts += 1;
            Err(DownloadFailure::ResponseTimeout(io::Error::new(
                io::ErrorKind::TimedOut,
                "WinHttpReceiveResponse timed out",
            )))
        });
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::TimedOut));
        assert_eq!(attempts, 2);
    }

    #[test]
    fn status_integrity_and_other_transport_failures_are_never_retried() {
        for (kind, message) in [
            (io::ErrorKind::Other, "model server returned HTTP 503"),
            (io::ErrorKind::InvalidData, "model SHA-256 mismatch"),
            (io::ErrorKind::InvalidData, "truncated model response"),
            (io::ErrorKind::TimedOut, "WinHttpSendRequest timed out"),
            (io::ErrorKind::TimedOut, "WinHttpReadData timed out"),
        ] {
            let mut attempts = 0;
            let result: io::Result<()> = retry_response_timeout(Instant::now(), || {
                attempts += 1;
                Err(io::Error::new(kind, message).into())
            });
            assert!(matches!(result, Err(error) if error.kind() == kind));
            assert_eq!(attempts, 1, "{message}");
        }
    }

    #[test]
    fn response_retry_does_not_reset_an_almost_exhausted_shared_budget() {
        let started = Instant::now() - DOWNLOAD_TIMEOUT + Duration::from_secs(9);
        let mut attempts = 0;
        let result: io::Result<()> = retry_response_timeout(started, || {
            attempts += 1;
            Err(DownloadFailure::ResponseTimeout(io::Error::new(
                io::ErrorKind::TimedOut,
                "WinHttpReceiveResponse timed out",
            )))
        });
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::TimedOut));
        assert_eq!(attempts, 1);
    }

    #[test]
    fn network_timeouts_shrink_with_the_budget_and_never_become_infinite() -> io::Result<()> {
        let started = Instant::now() - DOWNLOAD_TIMEOUT + Duration::from_secs(2);
        let timeouts = download_timeouts(started)?;
        assert!(
            timeouts
                .into_iter()
                .all(|timeout| (1..=2_000).contains(&timeout))
        );
        assert!(timeouts[..3].iter().sum::<i32>() <= 2_000);
        assert!(matches!(
            download_timeouts(Instant::now() - DOWNLOAD_TIMEOUT),
            Err(error) if error.kind() == io::ErrorKind::TimedOut
        ));
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "Requires an explicit hosted Windows network run with RUST_SWITCHER_HOSTED_E2E=1"]
    fn hosted_native_model_download_verifies_cache_and_reuses_it_offline() -> io::Result<()> {
        assert_eq!(
            std::env::var("RUST_SWITCHER_HOSTED_E2E").as_deref(),
            Ok("1"),
            "This network test must be explicitly enabled on a hosted Windows runner"
        );
        let german = MODELS
            .iter()
            .find(|spec| spec.code == "de")
            .ok_or_else(|| invalid_data("German model manifest is missing"))?;
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("de.fst");

        // Call the production WinHTTP implementation directly: test fixtures
        // must not satisfy this first load or hide a broken network path.
        let downloaded = load_cached_or_download(&path, german, || download_model(german))?;
        assert!(downloaded.contains_key("der"));
        let bytes = read_verified(&path, german)?;
        assert_eq!(bytes.len(), german.byte_len);

        let cached = load_cached_or_download(&path, german, || {
            Err(io::Error::other(
                "A verified cached model unexpectedly requested network access",
            ))
        })?;
        assert_eq!(cached.as_fst().as_bytes(), downloaded.as_fst().as_bytes());
        Ok(())
    }

    #[test]
    fn same_length_corrupt_cache_is_replaced_by_a_verified_model() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("en.fst");
        let mut corrupt = english_bytes().to_vec();
        corrupt[0] ^= 0xFF;
        fs::write(&path, &corrupt)?;
        assert!(read_verified(&path, english()).is_err());
        let loaded = load_cached_or_download(&path, english(), || Ok(english_bytes().to_vec()))?;
        assert!(loaded.contains_key("hello"));
        assert_eq!(read_verified(&path, english())?, english_bytes());
        Ok(())
    }

    #[test]
    fn truncated_or_wrong_download_is_never_published() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("en.fst");
        assert!(load_cached_or_download(&path, english(), || Ok(vec![0; 16])).is_err());
        assert!(!path.exists());

        let wrong = vec![0; english().byte_len];
        assert!(load_cached_or_download(&path, english(), || Ok(wrong)).is_err());
        assert!(!path.exists());
        Ok(())
    }

    #[test]
    fn cache_requests_are_deduplicated_and_retry_after_failure_is_bounded() {
        let now = Instant::now();
        let mut state = CacheState::default();
        assert!(state.request("de", now));
        assert!(!state.request("de", now));
        state.pending.remove("de");
        state.failed.insert("de", now);
        assert!(!state.request("de", now + Duration::from_secs(1)));
        assert!(state.request("de", now + RETRY_DELAY));
        assert!(state.request("uk", now));
    }

    #[test]
    fn manifest_length_limit_is_enforced_before_allocation() {
        let oversized = ModelSpec {
            byte_len: MAX_MODEL_BYTES + 1,
            ..*english()
        };
        assert!(verify_payload(&oversized, &[]).is_err());
        let empty = ModelSpec {
            byte_len: 0,
            ..*english()
        };
        assert!(verify_payload(&empty, &[]).is_err());
    }
}
