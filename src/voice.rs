use crate::discovery::config_dir;
use crate::models::{VoiceIntent, VoiceTranscribeRequest, VoiceTranscribeResponse};
use base64::{engine::general_purpose::STANDARD as BASE64_STD, Engine};
use std::fs::{self, File};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use uuid::Uuid;

/// Classifies spoken natural language into high-level agentic intents.
pub fn classify_voice_intent(raw_text: &str) -> VoiceIntent {
    let text = raw_text.trim();
    let lower = text.to_lowercase();

    if lower == "stop" || lower == "stop task" || lower == "cancel" || lower == "halt" {
        return VoiceIntent::StopTask;
    }

    if lower.contains("fix ci") || lower.contains("fix tests") || lower.contains("repair build") {
        return VoiceIntent::FixCi;
    }

    if lower.contains("run checks")
        || lower.contains("check ci")
        || lower.contains("pr status")
        || lower == "test"
    {
        return VoiceIntent::CheckCi;
    }

    if lower.contains("audit diff")
        || lower.contains("review diff")
        || lower.contains("second opinion")
    {
        return VoiceIntent::ReviewDiff;
    }

    if let Some(rest) = lower
        .strip_prefix("bounce to ")
        .or_else(|| lower.strip_prefix("handoff to "))
        .or_else(|| lower.strip_prefix("delegate to "))
    {
        let words: Vec<&str> = rest.split_whitespace().collect();
        if let Some(target) = words.first() {
            let instructions = if words.len() > 1 {
                Some(words[1..].join(" "))
            } else {
                None
            };
            return VoiceIntent::SwarmBounce {
                target_harness: target.to_string(),
                custom_instructions: instructions,
            };
        }
    }

    VoiceIntent::ChatPrompt(text.to_string())
}

/// Transcribes audio payload using local Whisper if available, with graceful client fallback.
pub fn process_voice_transcription(req: &VoiceTranscribeRequest) -> VoiceTranscribeResponse {
    // If client already transcribed via Web Speech API, classify and return directly
    if let Some(ref text) = req.speech_text {
        let clean = text.trim();
        if !clean.is_empty() {
            let intent = classify_voice_intent(clean);
            return VoiceTranscribeResponse {
                ok: true,
                transcript: clean.to_string(),
                intent,
                engine: "web_speech".to_string(),
            };
        }
    }

    // Server-side whisper fallback if audio bytes were sent
    if let Some(ref b64) = req.audio_base64 {
        if let Ok(bytes) = BASE64_STD.decode(b64.trim()) {
            if let Some(transcript) = try_local_whisper_transcription(&bytes, req.format.as_deref())
            {
                let intent = classify_voice_intent(&transcript);
                return VoiceTranscribeResponse {
                    ok: true,
                    transcript,
                    intent,
                    engine: "whisper".to_string(),
                };
            }
        }
    }

    VoiceTranscribeResponse {
        ok: true,
        transcript: String::new(),
        intent: VoiceIntent::ChatPrompt(String::new()),
        engine: "ready".to_string(),
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
    fn test_classify_voice_intent() {
        assert_eq!(classify_voice_intent("stop"), VoiceIntent::StopTask);
        assert_eq!(classify_voice_intent("stop task"), VoiceIntent::StopTask);
        assert_eq!(classify_voice_intent("halt"), VoiceIntent::StopTask);

        assert_eq!(
            classify_voice_intent("Please fix CI now"),
            VoiceIntent::FixCi
        );
        assert_eq!(classify_voice_intent("run checks"), VoiceIntent::CheckCi);
        assert_eq!(
            classify_voice_intent("review diff"),
            VoiceIntent::ReviewDiff
        );

        match classify_voice_intent("bounce to keel optimize buffer flush") {
            VoiceIntent::SwarmBounce {
                target_harness,
                custom_instructions,
            } => {
                assert_eq!(target_harness, "keel");
                assert_eq!(
                    custom_instructions,
                    Some("optimize buffer flush".to_string())
                );
            }
            _ => panic!("Expected SwarmBounce intent"),
        }

        assert_eq!(
            classify_voice_intent("Implement a new shader for the water system"),
            VoiceIntent::ChatPrompt("Implement a new shader for the water system".to_string())
        );
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
        assert_eq!(res.intent, VoiceIntent::FixCi);
        assert_eq!(res.engine, "web_speech");
    }
}
