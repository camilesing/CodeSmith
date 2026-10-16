//! Re-export of [`codesmith_agent_runtime::skills`] (Phase C6-1).
//!
//! Canonical home is now `codesmith_agent_runtime`. This shim keeps the
//! `crate::skills::<item>` paths that remain in the CLI working; the
//! explicit list below pins exactly which items this crate consumes, so
//! upstream renames fail here instead of at distant call sites and the
//! eventual removal is a mechanical check.
// TODO(shim-removal): remove this shim once the remaining call sites are
// rewired onto `codesmith_agent_runtime::skills` directly.
pub use codesmith_agent_runtime::skills::{
    Skill, SkillRegistry, agents_global_skills_dir, discover_for_workspace_and_dir,
    discover_from_directories, discover_in_workspace, install, install_system_skills,
    is_bundled_skill_name, preprocess_skill_body_for_activation,
    render_available_skills_context_with_registered, skills_directories, skills_from_registrations,
};
// Only the prompts.rs test helper consumes this through the shim today.
#[cfg(test)]
pub use codesmith_agent_runtime::skills::render_available_skills_context_for_workspace;
