//! Pluggable sandbox backend abstraction.
//!
//! External sandbox backends route shell command execution to a remote service
//! (e.g. Alibaba OpenSandbox) instead of spawning a local process. This is
//! complementary to the OS-level sandbox module (Seatbelt / Landlock / Windows)
//! — the external backend *replaces* local execution entirely when configured.
//!
//! The portable request/output/kind types and the `SandboxBackend` trait now
//! live in `codesmith-agent-runtime`'s `sandbox` module; this file re-exports
//! them and keeps the config-driven `create_backend` factory, which references
//! the CLI-local `Config` and `OpenSandboxBackend`.

use anyhow::Result;

pub use codesmith_sandbox::runtime::{
    FsIsolation, NetworkControl, SandboxBackend, SandboxBackendCapabilities, SandboxExecRequest,
    SandboxKind, SandboxOutput,
};

use crate::config::Config;

/// Create the configured sandbox backend from config.
///
/// Returns `None` when no external sandbox backend is configured (i.e. the
/// `sandbox_backend` key is absent, empty, or `"none"`). Container-family
/// ids (`docker`, `gvisor`, `firecracker`) construct the shared
/// [`ContainerCliBackend`](super::docker::ContainerCliBackend) with the
/// image from `[sandbox.docker] image` (default
/// [`DEFAULT_IMAGE`](super::docker::DEFAULT_IMAGE)); `"opensandbox"`
/// constructs an [`OpenSandboxBackend`](super::opensandbox::OpenSandboxBackend)
/// using `sandbox_url` and `sandbox_api_key`.
///
/// An unrecognized backend name is an error, not a silent fall-through to
/// local execution — a typo like `sandbox_backend = "opensandbox2"` must
/// never quietly disable the external sandbox the user asked for.
pub fn create_backend(config: &Config) -> Result<Option<Box<dyn SandboxBackend>>> {
    let raw = config
        .sandbox_backend
        .as_deref()
        .map(str::trim)
        .unwrap_or("");
    if raw.is_empty() {
        return Ok(None);
    }
    let Some(kind) = SandboxKind::parse(raw) else {
        anyhow::bail!(
            "unknown sandbox_backend {raw:?}: expected \"none\", \"docker\", \"gvisor\", \
             \"firecracker\" (alias kata-fc), or \"opensandbox\""
        );
    };

    match kind {
        SandboxKind::None => Ok(None),
        SandboxKind::Docker => Ok(Some(Box::new(super::docker::ContainerCliBackend::docker(
            container_image(config),
        )))),
        SandboxKind::Gvisor => Ok(Some(Box::new(super::docker::ContainerCliBackend::gvisor(
            container_image(config),
        )))),
        SandboxKind::Firecracker => Ok(Some(Box::new(
            super::docker::ContainerCliBackend::firecracker(container_image(config)),
        ))),
        SandboxKind::OpenSandbox => {
            // The documented default is kept, but never silently: the user
            // asked for a remote sandbox, and routing every command to
            // whatever listens on localhost:8080 must be visible up front
            // rather than surface later as "Failed to reach OpenSandbox".
            let base_url = config.sandbox_url.clone().unwrap_or_else(|| {
                tracing::warn!(
                    "sandbox_backend = \"opensandbox\" but sandbox_url is unset; \
                     defaulting to http://localhost:8080 — set sandbox_url explicitly"
                );
                "http://localhost:8080".to_string()
            });
            let api_key = config.sandbox_api_key.clone();
            let backend = super::opensandbox::OpenSandboxBackend::new(base_url, api_key, 30)?;
            Ok(Some(Box::new(backend)))
        }
    }
}

/// The `[sandbox.docker] image` value, or the documented default. Shared by
/// every container-family id (docker / gvisor / firecracker).
fn container_image(config: &Config) -> String {
    config
        .sandbox
        .as_ref()
        .and_then(|sandbox| sandbox.docker.as_ref())
        .and_then(|docker| docker.image.as_deref())
        .map(str::trim)
        .filter(|image| !image.is_empty())
        .unwrap_or(super::docker::DEFAULT_IMAGE)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(toml: &str) -> Config {
        toml::from_str(toml).expect("test config")
    }

    #[test]
    fn unknown_backend_fails_loud_with_the_full_id_set() {
        let err = create_backend(&config_with("sandbox_backend = \"opensandbox2\"\n"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown sandbox_backend"), "{err}");
        for id in ["none", "docker", "gvisor", "firecracker", "opensandbox"] {
            assert!(err.contains(id), "message must list {id}: {err}");
        }
    }

    #[test]
    fn kind_parse_accepts_the_documented_aliases() {
        use SandboxKind::{Docker, Firecracker, Gvisor, None as NoneKind, OpenSandbox};
        assert_eq!(SandboxKind::parse("docker"), Some(Docker));
        assert_eq!(SandboxKind::parse("gvisor"), Some(Gvisor));
        assert_eq!(SandboxKind::parse("runsc"), Some(Gvisor));
        assert_eq!(SandboxKind::parse("firecracker"), Some(Firecracker));
        assert_eq!(SandboxKind::parse("kata-fc"), Some(Firecracker));
        assert_eq!(SandboxKind::parse("opensandbox"), Some(OpenSandbox));
        assert_eq!(SandboxKind::parse("none"), Some(NoneKind));
        assert_eq!(SandboxKind::parse("bogus"), None);
    }

    #[test]
    fn container_ids_construct_with_distinct_capability_claims() {
        for (backend, id, linux_only) in [
            (
                create_backend(&config_with(
                    "sandbox_backend = \"docker\"\n[sandbox.docker]\nimage = \"debian:12\"\n",
                ))
                .unwrap()
                .expect("docker backend"),
                "docker",
                false,
            ),
            (
                create_backend(&config_with("sandbox_backend = \"gvisor\"\n"))
                    .unwrap()
                    .expect("gvisor backend"),
                "gvisor",
                true,
            ),
            (
                create_backend(&config_with("sandbox_backend = \"kata-fc\"\n"))
                    .unwrap()
                    .expect("firecracker backend via alias"),
                "firecracker",
                true,
            ),
        ] {
            assert_eq!(backend.id(), id);
            assert_eq!(backend.capabilities().requires_linux_host, linux_only);
            // Construction is cheap: no daemon probe happens here (that is
            // prepare(), at engine build).
            assert!(backend.task_plane().is_some());
        }
    }
}
