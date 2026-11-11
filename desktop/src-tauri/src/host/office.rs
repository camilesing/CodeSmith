//! The office document preview conversion (`officeToPdf/*`, the vendored
//! office renderer's doc/docx/ppt/pptx → PDF path): the bridge shells out to
//! a local LibreOffice `soffice` the way upstream's host bundles its own
//! conversion kit — no engine is downloaded, and an absent engine answers
//! the wire's `unavailable` reason so the panel renders its localized
//! unavailable state instead of pretending.
//!
//! `officeToPdf/render` reuses the workspace-files fence (`locate`'s
//! no-symlink, in-workspace gate) and re-stats the source around the
//! conversion (the opaque `version` token) so a file changed mid-flight
//! answers `source-changed`; the converted PDF is validated (`%PDF-` head,
//! `%%EOF` tail, size caps) and rides the same multipart bytes-attachment
//! mechanism `workspaceFiles/readBytes` uses. `officeToPdf/generation` is a
//! per-process UUID the client's preview cache keys on.
//!
//! Known limitations: no bridge-side result cache (the client caches by
//! `[generation, sessionId, absolutePath, version]`, so a repeat only pays
//! after a reload); no concurrency cap (every render gets its own isolated
//! soffice profile under the scratch dir, so concurrent conversions are safe
//! but unbounded); `priority` is accepted and ignored; a failing soffice
//! maps to `invalid-document` (its exit status carries no finer taxonomy);
//! the xls/xlsx extensions of the wire vocabulary are accepted but no
//! vendored client surface routes them here (the excel preview renders
//! those natively).
//! Env knobs (test/ops): `CODESMITH_DESKTOP_SOFFICE` names the converter
//! binary; `CODESMITH_DESKTOP_OFFICE_TIMEOUT_MS` overrides the 60s cap.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::files;
use super::HostState;

/// The wire vocabulary's office extensions (case-insensitive gate).
const OFFICE_EXTENSIONS: [&str; 6] = ["doc", "docx", "xls", "xlsx", "ppt", "pptx"];
/// Largest source the bridge converts (upstream's reserved byte budget role).
const MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;
/// Largest converted PDF the bridge serves (upstream's default).
const MAX_OUTPUT_BYTES: u64 = 100 * 1024 * 1024;
/// One conversion's wall clock cap.
const CONVERT_TIMEOUT: Duration = Duration::from_secs(60);

/// A conversion failure with its wire `reason`.
#[derive(Debug)]
struct RenderFail {
    reason: &'static str,
    message: String,
}

impl RenderFail {
    fn new(reason: &'static str, message: impl Into<String>) -> Self {
        Self {
            reason,
            message: message.into(),
        }
    }

    fn into_value(self) -> Value {
        json!({
            "ok": false,
            "error": {
                "code": "document-render/failed",
                "message": self.message,
                "details": { "reason": self.reason },
            }
        })
    }
}

/// The provider generation: one UUID per bridge process. The client keys its
/// preview cache on it, so every bridge restart invalidates cached PDFs.
pub fn generation_value() -> String {
    static GENERATION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    GENERATION
        .get_or_init(|| uuid::Uuid::new_v4().to_string())
        .clone()
}

/// Resolve the converter binary: the test/ops override first (even a missing
/// path — its spawn reports `unavailable`), then PATH, then the macOS
/// bundle's conventional home.
fn resolve_soffice() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("CODESMITH_DESKTOP_SOFFICE") {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    if let Ok(path) = which::which("soffice") {
        return Some(path);
    }
    #[cfg(target_os = "macos")]
    {
        let bundle = PathBuf::from("/Applications/LibreOffice.app/Contents/MacOS/soffice");
        if bundle.is_file() {
            return Some(bundle);
        }
    }
    None
}

/// `officeToPdf/render` — see the module doc. `Ok` carries the wire value
/// (with `data: null`) plus the PDF bytes the caller wraps as the multipart
/// attachment.
pub async fn render(host: &HostState, args: &Value) -> Result<(Value, Vec<u8>), Value> {
    render_inner(host, args)
        .await
        .map_err(|fail| fail.into_value())
}

async fn render_inner(host: &HostState, args: &Value) -> Result<(Value, Vec<u8>), RenderFail> {
    let session = args
        .get("workspaceFileScopeId")
        .and_then(Value::as_str)
        .ok_or_else(|| RenderFail::new("failed", "officeToPdf/render without session"))?;
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| RenderFail::new("failed", "officeToPdf/render without path"))?;
    let (target, metadata) = files::locate(host, session, path)
        .await
        .map_err(|err| RenderFail::new("failed", format!("render: {}", err.message)))?;
    if !metadata.is_file() {
        return Err(RenderFail::new(
            "failed",
            format!("render: {path} is not a file"),
        ));
    }
    let ext = target
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if !OFFICE_EXTENSIONS.contains(&ext.as_str()) {
        return Err(RenderFail::new(
            "unsupported-format",
            format!("render: .{ext} is not an office document"),
        ));
    }
    if metadata.len() > MAX_INPUT_BYTES {
        return Err(RenderFail::new(
            "input-too-large",
            format!("render: {path} is over the conversion cap"),
        ));
    }
    let Some(soffice) = resolve_soffice() else {
        return Err(RenderFail::new(
            "unavailable",
            "render: no LibreOffice (soffice) on this host",
        ));
    };
    let before = files::version_of(&metadata);
    let source = target.to_string_lossy().into_owned();
    let timeout = std::env::var("CODESMITH_DESKTOP_OFFICE_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(CONVERT_TIMEOUT);
    let converted =
        tokio::task::spawn_blocking(move || convert(&soffice, &source, &ext, &before, timeout))
            .await
            .map_err(|err| RenderFail::new("failed", format!("render join: {err}")))??;
    let value = json!({
        "absolutePath": target.to_string_lossy().replace('\\', "/"),
        "version": converted.version,
        "bytes": converted.pdf.len(),
        "offset": 0,
        "data": Value::Null,
        "eof": true,
        "missingFonts": [],
        "generation": generation_value(),
    });
    Ok((value, converted.pdf))
}

#[derive(Debug)]
struct Converted {
    pdf: Vec<u8>,
    version: String,
}

/// One conversion under a scratch directory, removed either way.
fn convert(
    soffice: &Path,
    source: &str,
    ext: &str,
    before: &str,
    timeout: Duration,
) -> Result<Converted, RenderFail> {
    let scratch = std::env::temp_dir().join(format!("cds-office-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&scratch)
        .map_err(|err| RenderFail::new("failed", format!("scratch: {err}")))?;
    let outcome = convert_in(&scratch, soffice, source, ext, before, timeout);
    let _ = std::fs::remove_dir_all(&scratch);
    outcome
}

fn convert_in(
    scratch: &Path,
    soffice: &Path,
    source: &str,
    ext: &str,
    before: &str,
    timeout: Duration,
) -> Result<Converted, RenderFail> {
    // Stage the source under the scratch dir (upstream's torn-read guard):
    // a version that moves during the copy answers source-changed.
    let stat = std::fs::symlink_metadata(source)
        .map_err(|err| RenderFail::new("failed", format!("stat source: {err}")))?;
    if stat.file_type().is_symlink() {
        return Err(RenderFail::new(
            "failed",
            "stat source: the final component is a symlink",
        ));
    }
    let staged_version = files::version_of(&stat);
    let staged = scratch.join(format!("source.{ext}"));
    std::fs::copy(source, &staged)
        .map_err(|err| RenderFail::new("failed", format!("stage source: {err}")))?;
    let restat = std::fs::symlink_metadata(source)
        .map_err(|err| RenderFail::new("failed", format!("restat source: {err}")))?;
    if staged_version != before || files::version_of(&restat) != before {
        return Err(RenderFail::new(
            "source-changed",
            "the source changed while staging the conversion",
        ));
    }

    let outdir = scratch.join("out");
    std::fs::create_dir_all(&outdir)
        .map_err(|err| RenderFail::new("failed", format!("scratch outdir: {err}")))?;
    let mut command = std::process::Command::new(soffice);
    command
        .arg("--headless")
        .arg("--norestore")
        // An isolated profile per conversion: no shared profile lock, and no
        // dependency on the user's HOME.
        .arg(format!(
            "-env:UserInstallation=file://{}",
            scratch.join("profile").display()
        ))
        .arg("--convert-to")
        .arg("pdf")
        .arg("--outdir")
        .arg(&outdir)
        .arg(&staged);
    command.env_clear();
    command.env("HOME", scratch);
    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::null());
    command.stderr(std::process::Stdio::null());
    let mut child = command.spawn().map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            RenderFail::new("unavailable", "soffice not found on this host")
        } else {
            RenderFail::new("failed", format!("spawn soffice: {err}"))
        }
    })?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(RenderFail::new(
                        "timeout",
                        "soffice did not finish within the conversion timeout",
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(err) => {
                return Err(RenderFail::new("failed", format!("wait soffice: {err}")));
            }
        }
    };
    if !status.success() {
        // soffice's exit status carries no taxonomy; the generic conversion
        // failure is the honest mapping.
        return Err(RenderFail::new(
            "invalid-document",
            format!("soffice exited with {status}"),
        ));
    }

    let pdf_path = outdir.join("source.pdf");
    let pdf_stat = std::fs::symlink_metadata(&pdf_path)
        .map_err(|_| RenderFail::new("invalid-output", "soffice produced no output"))?;
    if pdf_stat.file_type().is_symlink() || !pdf_stat.is_file() {
        return Err(RenderFail::new(
            "invalid-output",
            "the conversion output is not a regular file",
        ));
    }
    if pdf_stat.len() > MAX_OUTPUT_BYTES {
        return Err(RenderFail::new(
            "output-too-large",
            "the converted PDF is over the cap",
        ));
    }
    let pdf = std::fs::read(&pdf_path)
        .map_err(|err| RenderFail::new("failed", format!("read output: {err}")))?;
    if !pdf.starts_with(b"%PDF-")
        || !pdf[pdf.len().saturating_sub(1024)..]
            .windows(5)
            .any(|w| w == b"%%EOF")
    {
        return Err(RenderFail::new(
            "invalid-output",
            "the conversion output is not a PDF",
        ));
    }

    // The source must be unchanged by the time the PDF exists.
    let after = std::fs::symlink_metadata(source)
        .map_err(|_| RenderFail::new("source-changed", "the source vanished mid-conversion"))?;
    let version = files::version_of(&after);
    if version != before {
        return Err(RenderFail::new(
            "source-changed",
            "the source changed while converting",
        ));
    }
    Ok(Converted { pdf, version })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A converter stand-in: parse `--outdir <dir>` and write a minimal PDF.
    fn fake_soffice(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("soffice-fake");
        let script = format!(
            "#!/bin/sh\nout=\nprev=\nfor a in \"$@\"; do\n  if [ \"$prev\" = \"--outdir\" ]; then out=\"$a\"; fi\n  prev=\"$a\"\ndone\n{body}\n"
        );
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    fn pdf_bytes() -> Vec<u8> {
        b"%PDF-1.4\nfake body\n%%EOF\n".to_vec()
    }

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("cds-office-test-{tag}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn convert_runs_soffice_and_returns_the_validated_pdf() {
        let scratch = Scratch::new("ok");
        let source = scratch.0.join("source.docx");
        std::fs::write(&source, b"pretend docx").unwrap();
        let stat = std::fs::symlink_metadata(&source).unwrap();
        let before = files::version_of(&stat);
        let soffice = fake_soffice(
            &scratch.0,
            "printf '%%PDF-1.4\\nfake body\\n%%%%EOF\\n' > \"$out/source.pdf\"",
        );
        let converted = convert(
            &soffice,
            source.to_str().unwrap(),
            "docx",
            &before,
            CONVERT_TIMEOUT,
        )
        .expect("conversion succeeds");
        assert_eq!(converted.pdf, pdf_bytes());
        assert_eq!(converted.version, before);
    }

    #[cfg(unix)]
    #[test]
    fn convert_maps_engine_failures_to_the_wire_reasons() {
        let scratch = Scratch::new("reasons");
        let source = scratch.0.join("source.docx");
        std::fs::write(&source, b"pretend docx").unwrap();
        let stat = std::fs::symlink_metadata(&source).unwrap();
        let before = files::version_of(&stat);

        // A failing engine is an invalid document.
        let soffice = fake_soffice(&scratch.0, "exit 1");
        let reason = convert(
            &soffice,
            source.to_str().unwrap(),
            "docx",
            &before,
            CONVERT_TIMEOUT,
        )
        .unwrap_err()
        .reason;
        assert_eq!(reason, "invalid-document");

        // Garbage output is an invalid output.
        let soffice = fake_soffice(&scratch.0, "printf 'not a pdf' > \"$out/source.pdf\"");
        let reason = convert(
            &soffice,
            source.to_str().unwrap(),
            "docx",
            &before,
            CONVERT_TIMEOUT,
        )
        .unwrap_err()
        .reason;
        assert_eq!(reason, "invalid-output");

        // A stalled engine times out and is killed.
        let soffice = fake_soffice(&scratch.0, "sleep 5");
        let reason = convert(
            &soffice,
            source.to_str().unwrap(),
            "docx",
            &before,
            Duration::from_millis(150),
        )
        .unwrap_err()
        .reason;
        assert_eq!(reason, "timeout");

        // A missing engine is unavailable.
        let missing = scratch.0.join("no-such-soffice");
        let reason = convert(
            &missing,
            source.to_str().unwrap(),
            "docx",
            &before,
            CONVERT_TIMEOUT,
        )
        .unwrap_err()
        .reason;
        assert_eq!(reason, "unavailable");

        // A version token that moved before the conversion is source-changed.
        let soffice = fake_soffice(
            &scratch.0,
            "printf '%%PDF-1.4\\nx\\n%%%%EOF\\n' > \"$out/source.pdf\"",
        );
        let reason = convert(
            &soffice,
            source.to_str().unwrap(),
            "docx",
            "0:0",
            CONVERT_TIMEOUT,
        )
        .unwrap_err()
        .reason;
        assert_eq!(reason, "source-changed");
    }

    #[test]
    fn generation_is_stable_within_the_process() {
        assert_eq!(generation_value(), generation_value());
    }
}
