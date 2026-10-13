//! Multi-judge visual critique panel (0065 D7, #539 §2).
//!
//! Runs a lineup of local llama.cpp vision judges against one frame and
//! collects their verdicts. Deliberately surface-agnostic: the input is a
//! PNG/JPEG path plus a display label — `critique --panel <surface>`
//! feeds it a fresh session capture, `critique --panel --image <path>`
//! feeds it any frame on disk.
//!
//! Judge lineup lives in a user config (machine-specific model paths must
//! not be repo state): `$HOME/.config/dashboard/panel-judges.json`, overridable
//! with `--judges`. See `crates/debug/verify/panel-judges.example.json`.

use serde::Deserialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// One judge from `panel-judges.json`.
#[derive(Deserialize, Clone)]
pub struct JudgeConfig {
    pub name: String,
    pub model: PathBuf,
    /// Multimodal projector; omitted for text-only judges.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mmproj: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Merged into the chat-completions request body — per-judge request
    /// tuning (temperature, `chat_template_kwargs` like MiniCPM's
    /// `{"enable_thinking": false}`: the model card's fast mode is the
    /// documented default; thinking is opt-in for complex tasks).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overrides: Option<serde_json::Value>,
}

#[derive(Deserialize, Default)]
struct ServerConfig {
    port: Option<u16>,
    /// Bounded context — llama-server's default provisions the model's
    /// full KV cache and OOMs Metal on 24GB machines (#539 §5 lesson).
    context: Option<u32>,
}

#[derive(Deserialize)]
struct PanelConfig {
    judges: Vec<JudgeConfig>,
    #[serde(default)]
    server: ServerConfig,
}

/// The built-in brief. `{label}` is replaced per run. Blind-refusal check
/// first: a judge that admits it sees nothing must say so and stop.
pub const BRIEF_TEMPLATE: &str = "You are a harsh senior product designer reviewing a desktop dashboard app screen (1280x800): the \"{label}\" surface. If you cannot see an image, say exactly that and nothing else. If you can: critique it as visual design — hierarchy, spacing rhythm, density, color restraint, typography, alignment, affordance clarity, composition. Judge against the standard of the best desktop apps (Linear, Raycast, Zed, GitHub desktop): would this pass as \"nice\", or only as \"clean\"? Name the 5 worst concrete problems (each with WHERE and a specific fix), then the single best element. Findings must reference what you actually see — no generic advice.";

pub struct PanelInvocation {
    pub image: PathBuf,
    pub label: String,
    pub out_dir: PathBuf,
    pub judges_path: PathBuf,
    /// `--brief <path>`: a file whose contents replace the built-in
    /// template (`{label}` still substituted).
    pub brief_path: Option<PathBuf>,
}

/// Default judge-lineup location.
pub fn default_judges_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home)
        .join(".config/dashboard/panel-judges.json")
        .to_path_buf()
}

fn curl(args: &[&str]) -> Result<String, String> {
    let out = Command::new("curl")
        .args(args)
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| format!("cannot run curl (the panel shells out to it): {err}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// One judge, one llama-server, one request. Returns (verdict bytes, duration).
/// Mime for the frame as sent to the judges. JPEG is the tested-everywhere
/// path; MiniCPM's projector has been observed blind on PNG data-URIs
/// (#543) — guarded by [`blind_refusal`] and a manifest note.
fn mime_for(image: &Path) -> &'static str {
    match image.extension().and_then(|e| e.to_str()) {
        Some("png") => "image/png",
        _ => "image/jpeg",
    }
}

/// Blind-refusal markers: phrases a vision judge emits when it never
/// received the frame. Case-insensitive, matched against the verdict —
/// a judge that answered without seeing must not count as a
/// finding-free run, because a silent blind judge poisons cluster
/// counts (0065 D7, #543).
fn blind_refusal(verdict: &str) -> Option<&'static str> {
    const MARKERS: [&str; 13] = [
        "cannot see an image",
        "can't see an image",
        "could not see an image",
        "could not see the image",
        "unable to see an image",
        "unable to see the image",
        "no image attached",
        "no image was provided",
        "no image provided",
        "please provide an image",
        "please provide the image",
        "don't see any image",
        "do not see any image",
    ];
    if verdict.trim().is_empty() {
        return Some("empty verdict");
    }
    let lower = verdict.to_lowercase();
    MARKERS.into_iter().find(|marker| lower.contains(marker))
}

fn run_judge(
    judge: &JudgeConfig,
    server: &ServerConfig,
    image: &Path,
    brief: &str,
    out_dir: &Path,
) -> Result<(String, Duration), String> {
    let port = server.port.unwrap_or(8081);
    let context = server.context.unwrap_or(8192);
    for binary in ["llama-server", "curl"] {
        if Command::new(binary)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_err()
        {
            return Err(format!("{binary} is not on PATH — the panel needs it"));
        }
    }
    let log = out_dir.join(format!("{}.server.log", judge.name));
    let log_file = std::fs::File::create(&log)
        .map_err(|err| format!("cannot create {}: {err}", log.display()))?;
    let mut server_args = vec![
        "-m".into(),
        judge.model.display().to_string(),
        "--port".into(),
        port.to_string(),
        // Bounded context + fuzzed buffers: the #539 §5 OOM lesson. Never
        // let llama-server provision the model's full KV cache.
        "-c".into(),
        context.to_string(),
        "-np".into(),
        "1".into(),
        "-fa".into(),
        "on".into(),
        "-b".into(),
        "512".into(),
        "-ub".into(),
        "256".into(),
        "-ngl".into(),
        "99".into(),
    ];
    if let Some(mmproj) = &judge.mmproj {
        server_args.push("--mmproj".into());
        server_args.push(mmproj.display().to_string());
    }
    let mut child: Child = Command::new("llama-server")
        .args(&server_args)
        .stdout(Stdio::from(
            log_file.try_clone().map_err(|e| e.to_string())?,
        ))
        .stderr(Stdio::from(log_file))
        .spawn()
        .map_err(|err| format!("cannot spawn llama-server: {err}"))?;
    let result = drive_judge(&mut child, server, image, brief, out_dir, judge);
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn drive_judge(
    child: &mut Child,
    server: &ServerConfig,
    image: &Path,
    brief: &str,
    out_dir: &Path,
    judge: &JudgeConfig,
) -> Result<(String, Duration), String> {
    let started = Instant::now();
    let port = server.port.unwrap_or(8081);
    let base = format!("http://127.0.0.1:{port}");
    // Model load into Metal takes tens of seconds; five minutes is the
    // ceiling, cold page cache included.
    let deadline = Instant::now() + Duration::from_secs(300);
    let mut healthy = false;
    while Instant::now() < deadline {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!(
                "llama-server exited before becoming healthy (status {status:?}); see the .server.log next to the verdicts"
            ));
        }
        if curl(&["-sf", &format!("{base}/health")]).is_ok() {
            healthy = true;
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    if !healthy {
        return Err("llama-server never became healthy within 300s".into());
    }

    let b64 = {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(image)
            .and_then(|mut f| f.read_to_end(&mut bytes))
            .map_err(|err| format!("cannot read {}: {err}", image.display()))?;
        // Standard base64 without a dependency: 3 bytes → 4 chars.
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
            const T: &[u8; 64] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            out.push(T[(n >> 18) as usize & 63] as char);
            out.push(T[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 {
                T[(n >> 6) as usize & 63] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                T[n as usize & 63] as char
            } else {
                '='
            });
        }
        out
    };

    let mime = mime_for(image);
    let payload = serde_json::json!({
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": brief},
                {"type": "image_url", "image_url": {"url": format!("data:{mime};base64,{b64}")}}
            ]
        }],
        "max_tokens": judge.max_tokens.unwrap_or(2000),
        "temperature": 0.3,
        "stream": false,
    });
    let payload = match &judge.overrides {
        Some(extra) => {
            let serde_json::Value::Object(extra) = extra else {
                return Err("judge overrides must be a JSON object".into());
            };
            let mut merged = payload;
            for (key, value) in extra {
                merged[key] = value.clone();
            }
            merged
        }
        None => payload,
    };
    let body_path = out_dir.join(format!(".request-{}.json", judge.name));
    std::fs::write(
        &body_path,
        serde_json::to_vec(&payload).expect("serialize request"),
    )
    .map_err(|err| format!("cannot write {}: {err}", body_path.display()))?;

    let body = curl(&[
        "-sS",
        "--fail",
        "--max-time",
        "600",
        "-X",
        "POST",
        "-H",
        "content-type: application/json",
        "--data-binary",
        &format!("@{}", body_path.display()),
        &format!("{base}/v1/chat/completions"),
    ])?;
    let response: serde_json::Value =
        serde_json::from_str(&body).map_err(|err| format!("unparseable response: {err}"))?;
    let message = &response["choices"][0]["message"];
    // Thinking models can spend the whole budget in <think>; the fallback
    // keeps the verdict file non-empty (the #539 §5 shakedown lesson).
    let content = message["content"]
        .as_str()
        .filter(|text| !text.is_empty())
        .or_else(|| message["reasoning_content"].as_str())
        .unwrap_or_default();
    if content.is_empty() {
        return Err("response carried no content and no reasoning_content".into());
    }
    Ok((content.to_string(), started.elapsed()))
}

/// Run the panel. Returns a process exit code; verdicts land as
/// `<out_dir>/<judge>.txt` (+ `.err` on failure) with a `run.json` manifest.
pub fn run(invocation: &PanelInvocation) -> i32 {
    let image = invocation.image.as_path();
    if !image.is_file() {
        eprintln!("critique --panel: no such image: {}", image.display());
        return 1;
    }
    let mime = mime_for(image);
    if mime != "image/jpeg" {
        eprintln!(
            "panel: note: {} is {mime}, not JPEG — MiniCPM-class projectors have gone blind on PNG frames; verdicts are refusal-checked (#543)",
            image.display()
        );
    }
    let config_text = match std::fs::read_to_string(&invocation.judges_path) {
        Ok(text) => text,
        Err(err) => {
            eprintln!(
                "critique --panel: cannot read judge lineup {}: {err}\n  copy crates/debug/verify/panel-judges.example.json there and point `model`/`mmproj` at your local GGUFs",
                invocation.judges_path.display()
            );
            return 1;
        }
    };
    let config: PanelConfig = match serde_json::from_str(&config_text) {
        Ok(config) => config,
        Err(err) => {
            eprintln!(
                "critique --panel: bad lineup {}: {err}",
                invocation.judges_path.display()
            );
            return 1;
        }
    };
    if config.judges.is_empty() {
        eprintln!("critique --panel: lineup has no judges");
        return 1;
    }
    let brief_template = match &invocation.brief_path {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) => {
                eprintln!(
                    "critique --panel: cannot read brief {}: {err}",
                    path.display()
                );
                return 1;
            }
        },
        None => BRIEF_TEMPLATE.to_string(),
    };
    let brief = brief_template.replace("{label}", &invocation.label);
    if let Err(err) = std::fs::create_dir_all(&invocation.out_dir) {
        eprintln!(
            "critique --panel: cannot create {}: {err}",
            invocation.out_dir.display()
        );
        return 1;
    }
    let out_dir = invocation.out_dir.as_path();
    let mut manifest = serde_json::json!({
        "label": invocation.label,
        "image": image.display().to_string(),
        "image_bytes": image.metadata().map(|m| m.len()).unwrap_or(0),
        "image_mime": mime,
        "blind_risk": mime != "image/jpeg",
        "brief_source": invocation
            .brief_path
            .as_deref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "builtin".into()),
        "lineup": invocation.judges_path.display().to_string(),
        "judges": [],
    });
    let mut any = false;
    for judge in &config.judges {
        println!("=== judge: {}", judge.name);
        let verdict = out_dir.join(format!("{}.txt", judge.name));
        let err_path = out_dir.join(format!("{}.err", judge.name));
        match run_judge(judge, &config.server, image, &brief, &invocation.out_dir) {
            Ok((text, duration)) => {
                if let Some(marker) = blind_refusal(&text) {
                    let _ = std::fs::write(&verdict, &text); // evidence stays
                    let err = format!(
                        "blind refusal — verdict matched {marker:?}; the judge likely never saw the frame (JPEG is the tested path, #543)"
                    );
                    let _ = std::fs::write(&err_path, format!("{err}\n"));
                    eprintln!("panel: {} BLIND: {err}", judge.name);
                    manifest["judges"]
                        .as_array_mut()
                        .expect("array")
                        .push(serde_json::json!({
                            "name": judge.name,
                            "ok": false,
                            "blind": true,
                            "marker": marker,
                            "duration_ms": duration.as_millis() as u64,
                        }));
                    continue;
                }
                any = true;
                if let Err(err) = std::fs::write(&verdict, &text) {
                    eprintln!("cannot write {}: {err}", verdict.display());
                    return 1;
                }
                println!("panel: {} done ({:.0?})", judge.name, duration);
                manifest["judges"].as_array_mut().expect("array").push(
                    serde_json::json!({"name": judge.name, "ok": true, "duration_ms": duration.as_millis() as u64}),
                );
            }
            Err(err) => {
                let _ = std::fs::write(&err_path, format!("{err}\n"));
                let _ = std::fs::File::create(&verdict); // empty: loud in listings
                eprintln!("panel: {} FAILED: {err}", judge.name);
                manifest["judges"]
                    .as_array_mut()
                    .expect("array")
                    .push(serde_json::json!({"name": judge.name, "ok": false, "error": err}));
            }
        }
    }
    let manifest_path = invocation.out_dir.join("run.json");
    if let Ok(body) = serde_json::to_string_pretty(&manifest) {
        let _ =
            std::fs::File::create(&manifest_path).and_then(|mut f| f.write_all(body.as_bytes()));
    }
    if !any {
        eprintln!(
            "panel: every judge failed — verdicts in {}",
            invocation.out_dir.display()
        );
        return 1;
    }
    println!(
        "panel: complete — verdicts in {}",
        invocation.out_dir.display()
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lineup_parses_overrides_and_optional_mmproj() {
        let config: PanelConfig = serde_json::from_str(
            r#"{"judges": [
                {"name": "text-only", "model": "/tmp/a.gguf", "max_tokens": 900},
                {"name": "vision", "model": "/tmp/b.gguf", "mmproj": "/tmp/b.mmproj",
                 "overrides": {"chat_template_kwargs": {"enable_thinking": false}}}
            ]}"#,
        )
        .expect("parse lineup");
        assert_eq!(config.judges.len(), 2);
        assert!(config.judges[0].mmproj.is_none());
        assert_eq!(config.judges[0].max_tokens, Some(900));
        assert!(config.judges[1].overrides.is_some());
        assert_eq!(config.server.port, None);
    }

    #[test]
    fn builtin_brief_carries_label_slot_and_refusal_check() {
        let brief = BRIEF_TEMPLATE.replace("{label}", "home");
        assert!(brief.contains("the \"home\" surface"));
        assert!(brief.contains("If you cannot see an image"));
        assert!(BRIEF_TEMPLATE.contains("{label}")); // placeholder pre-substitution
    }
    #[test]
    fn blind_refusal_flags_the_minicpm_png_reply() {
        // Verbatim from the exp2 run that motivated #543.
        let verdict = "I cannot see an image.\n\nIf you provide an image, I can \
                       critique it as requested.";
        assert_eq!(blind_refusal(verdict), Some("cannot see an image"));
        assert_eq!(blind_refusal("   \n\t"), Some("empty verdict"));
        assert_eq!(
            blind_refusal("Please provide the image again."),
            Some("please provide the image")
        );
    }

    #[test]
    fn blind_refusal_spares_real_critiques() {
        let verdict = "1. WHERE: The palette swatches lack affordance. \
                       The image shows Gradient start and Gradient end as \
                       identical squares — give each swatch a hairline border. \
                       The single best element is the size ladder.";
        assert_eq!(blind_refusal(verdict), None);
    }

    #[test]
    fn mime_follows_the_frame_extension() {
        assert_eq!(mime_for(Path::new("frames/a.png")), "image/png");
        assert_eq!(mime_for(Path::new("frames/a.jpg")), "image/jpeg");
        assert_eq!(mime_for(Path::new("frames/a.jpeg")), "image/jpeg");
        assert_eq!(mime_for(Path::new("frames/noext")), "image/jpeg");
    }
}
