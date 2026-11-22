use crate::discovery::config_dir;
use crate::models::{VoiceIntent, VoiceTranscribeRequest, VoiceTranscribeResponse};
use base64::{engine::general_purpose::STANDARD as BASE64_STD, Engine};
use std::fs::{self, File};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use uuid::Uuid;

/// Transcribes audio payload using local Whisper if available, with graceful client fallback.
pub fn process_voice_transcription(req: &VoiceTranscribeRequest) -> VoiceTranscribeResponse {
    // Dictation is text for review, never an instruction to execute an action.
    if let Some(ref text) = req.speech_text {
        let clean = text.trim();
        if !clean.is_empty() {
            let intent = VoiceIntent::ChatPrompt(clean.to_string());
            return VoiceTranscribeResponse {
                ok: true,
                transcript: clean.to_string(),
                intent,
                engine: "web_speech".to_string(),
                error: None,
            };
        }
    }

    // Server-side whisper fallback if audio bytes were sent
    if let Some(ref b64) = req.audio_base64 {
        if let Ok(bytes) = BASE64_STD.decode(b64.trim()) {
            if let Some(transcript) = try_local_whisper_transcription(&bytes, req.format.as_deref())
            {
                let intent = VoiceIntent::ChatPrompt(transcript.clone());
                return VoiceTranscribeResponse {
                    ok: true,
                    transcript,
                    intent,
                    engine: "whisper".to_string(),
                    error: None,
                };
            }
        }
    }

    VoiceTranscribeResponse {
        ok: false,
        transcript: String::new(),
        intent: VoiceIntent::ChatPrompt(String::new()),
        engine: "unavailable".to_string(),
        error: Some(
            "No transcript was produced. Try again or use your keyboard microphone.".into(),
        ),
    }
}

fn safe_audio_extension(format: Option<&str>) -> String {
    const ALLOWED: [&str; 8] = ["wav", "mp3", "m4a", "ogg", "webm", "mp4", "aiff", "flac"];
    format
        .map(|f| f.trim().to_ascii_lowercase())
        .filter(|f| ALLOWED.contains(&f.as_str()))
        .unwrap_or_else(|| "wav".to_string())
}

/// Runs a subprocess with a hard timeout, writing stdout/stderr into files.
/// Piping them instead deadlocks whisper once the 64 KiB OS pipe buffer fills
/// (its per-segment progress chatter), which the previous `try_wait()`-only
/// loop hit deterministically on long clips: the child blocked, the poll
/// never saw an exit, and the whole timeout budget burned for nothing.
fn run_with_timeout(
    mut cmd: Command,
    secs: u64,
    stdout_path: &Path,
    stderr_path: &Path,
) -> Option<std::process::ExitStatus> {
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let out_file = File::create(stdout_path).ok()?;
    let err_file = File::create(stderr_path).ok()?;
    cmd.stdout(Stdio::from(out_file))
        .stderr(Stdio::from(err_file))
        .stdin(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {}
            Err(_) => return None,
        }
        if start.elapsed() >= Duration::from_secs(secs) {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Server-side whisper must complete within this budget per candidate or it
/// is dropped and the client falls back to manual typing.
const WHISPER_TIMEOUT_SECS: u64 = 45;
/// Whisper spawns are heavy (model load per call) and run on the finite
/// blocking pool; cap concurrent transcriptions so voice spam can't starve
/// every other blocking user in the process.
const WHISPER_MAX_CONCURRENT: usize = 2;
static WHISPER_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// Delete leftovers older than an hour in `tmp_audio` (transcripts, audio,
/// sidecars, per-attempt stdout captures) so the dir cannot accumulate user
/// voice data forever.
fn sweep_stale_tmp_audio(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    for entry in entries.flatten() {
        let md = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if md.is_file() {
            if let Ok(modified) = md.modified() {
                if modified < cutoff {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
    }
}

fn try_local_whisper_transcription(audio_bytes: &[u8], format: Option<&str>) -> Option<String> {
    let guard = WHISPER_IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
    struct Decr;
    impl Drop for Decr {
        fn drop(&mut self) {
            WHISPER_IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
        }
    }
    let _guard = Decr;
    if guard >= WHISPER_MAX_CONCURRENT {
        // Already two transcriptions running: skip whisper entirely, the
        // client keeps its Web Speech / typing fallback.
        return None;
    }

    let ext = safe_audio_extension(format);
    let tmp_id = Uuid::new_v4().simple();
    let tmp_dir = config_dir().join("tmp_audio");
    let _ = fs::create_dir_all(&tmp_dir);
    sweep_stale_tmp_audio(&tmp_dir);
    // `ext` is whitelist-validated above, so the tmp path can never escape
    // tmp_audio even though it is composed from request input.
    let tmp_file = tmp_dir.join(format!("{}.{}", tmp_id, ext));

    if fs::write(&tmp_file, audio_bytes).is_err() {
        return None;
    }

    // whisper.cpp `--output-txt` drops a sidecar transcript next to the input;
    // track it so it never lingers with spoken content on disk.
    let sidecar_txt = tmp_dir.join(format!("{}.txt", tmp_id));
    let whisper_candidates = ["whisper-cli", "whisper", "whisper.cpp"];
    let mut transcript = None;
    for bin in &whisper_candidates {
        if transcript.is_some() {
            break;
        }
        let stdout_path = tmp_dir.join(format!("{}.{}.stdout", tmp_id, bin.replace('.', "_")));
        let stderr_path = tmp_dir.join(format!("{}.{}.stderr", tmp_id, bin.replace('.', "_")));
        let mut cmd = Command::new(bin);
        cmd.arg(&tmp_file)
            .arg("--model")
            .arg("base")
            .arg("--output-txt");
        let status = run_with_timeout(cmd, WHISPER_TIMEOUT_SECS, &stdout_path, &stderr_path);
        let ok = status.map(|s| s.success()).unwrap_or(false);
        if ok {
            // Prefer the captured stdout; fall back to the sidecar txt.
            let stdout = fs::read_to_string(&stdout_path)
                .unwrap_or_default()
                .trim()
                .to_string();
            if !stdout.is_empty() {
                transcript = Some(stdout);
            } else if let Ok(txt) = fs::read_to_string(&sidecar_txt) {
                let t = txt.trim().to_string();
                if !t.is_empty() {
                    transcript = Some(t);
                }
            }
        }
        let _ = fs::remove_file(&stdout_path);
        let _ = fs::remove_file(&stderr_path);
        // Note: the audio tmp_file must survive until here — the old code
        // deleted it *before* trying the next candidate, guaranteeing the
        // remaining candidates failed on a nonexistent input.
    }

    let _ = fs::remove_file(&tmp_file);
    let _ = fs::remove_file(&sidecar_txt);
    transcript
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safe_audio_extension() {
        assert_eq!(safe_audio_extension(Some("mp3")), "mp3");
        assert_eq!(safe_audio_extension(Some("WAV")), "wav");
        assert_eq!(safe_audio_extension(None), "wav");
        // Path traversal attempts collapse to "wav" — the tmp file stays
        // inside tmp_audio.
        assert_eq!(safe_audio_extension(Some("../evil")), "wav");
        assert_eq!(safe_audio_extension(Some("exe")), "wav");
        assert_eq!(safe_audio_extension(Some("wav/../x")), "wav");
        assert_eq!(safe_audio_extension(Some("xtremelongext")), "wav");
    }

    #[test]
    fn empty_or_invalid_audio_is_a_visible_failure() {
        for audio_base64 in [None, Some("not-base64!".to_string())] {
            let response = process_voice_transcription(&VoiceTranscribeRequest {
                audio_base64,
                format: Some("wav".into()),
                speech_text: None,
            });
            assert!(!response.ok);
            assert!(response.error.is_some());
            assert!(response.transcript.is_empty());
        }
    }

    #[test]
    fn test_process_voice_transcription_web_speech() {
        let req = VoiceTranscribeRequest {
            audio_base64: None,
            format: None,
            speech_text: Some("fix tests".to_string()),
        };
        let res = process_voice_transcription(&req);
        assert!(res.ok);
        assert_eq!(res.transcript, "fix tests");
        assert_eq!(res.intent, VoiceIntent::ChatPrompt("fix tests".into()));
        assert_eq!(res.engine, "web_speech");
    }
}
