//! Bundled instructions work without a checkout, configuration or daemon.
use std::path::Path;
use std::time::Duration;

use assert_cmd::Command;
use predicates::prelude::*;

fn command(binary: &Path, directory: &Path) -> Command {
    let mut command = Command::new(binary);
    for name in [
        "NITS_CONTEXT",
        "NITS_CONFIG",
        "NITS_SOCKET",
        "NITS_WS_URL",
        "NITS_DATA_DIR",
        "NITS_AGENT",
        "NITS_USER",
    ] {
        command.env_remove(name);
    }
    command
        .current_dir(directory)
        .env("XDG_CONFIG_HOME", directory.join("config-home"))
        .env("XDG_DATA_HOME", directory.join("data-home"))
        .timeout(Duration::from_secs(3));
    command
}

#[test]
fn standalone_skill_ignores_broken_configuration_and_never_connects_or_creates_state() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("nits");
    std::fs::copy(assert_cmd::cargo::cargo_bin("nits"), &binary).unwrap();
    let config = dir.path().join("broken.toml");
    let broken = "this is not = valid [ toml";
    std::fs::write(&config, broken).unwrap();
    let socket = dir.path().join("daemon.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut outputs = Vec::new();
    for args in [
        vec!["skill"],
        vec!["--json", "skill"],
        vec![
            "--config",
            config.to_str().unwrap(),
            "-c",
            "missing-context",
            "skill",
        ],
        vec!["--socket", socket.to_str().unwrap(), "skill"],
        vec!["--daemon-url", "ws://127.0.0.1:1", "skill"],
        vec![
            "--data-dir",
            dir.path().join("never-created").to_str().unwrap(),
            "skill",
        ],
    ] {
        let output = command(&binary, dir.path())
            .args(args)
            .assert()
            .success()
            .stderr("")
            .get_output()
            .stdout
            .clone();
        outputs.push(output);
    }
    for pair in outputs.windows(2) {
        assert_eq!(pair[0], pair[1]);
    }
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert_eq!(std::fs::read_to_string(config).unwrap(), broken);
    let mut entries = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    entries.sort();
    assert_eq!(entries, ["broken.toml", "daemon.sock", "nits"]);
}

#[test]
fn output_preserves_canonical_instructions_and_inlines_every_required_reference() {
    let dir = tempfile::tempdir().unwrap();
    let output = command(&assert_cmd::cargo::cargo_bin("nits"), dir.path())
        .arg("skill")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let output = String::from_utf8(output).unwrap();
    assert!(output.starts_with("---\nname: nits-review\n"));
    assert!(output.contains(&format!("Bundled with Nits {}", env!("CARGO_PKG_VERSION"))));
    assert!(!output.contains('\u{1b}'));
    // These are the canonical files reached through the package-local symlink
    // in source, and regular bundled files when this package is extracted.
    for source in [
        include_str!("../bundled-skill/SKILL.md"),
        include_str!("../bundled-skill/references/interaction.md"),
        include_str!("../bundled-skill/references/revisions.md"),
    ] {
        for line in source
            .lines()
            .filter(|line| !line.is_empty() && !line.contains("]("))
        {
            assert!(
                output.lines().any(|rendered| rendered == line),
                "missing canonical line: {line}"
            );
        }
    }
    assert!(
        !output.contains("(references/")
            && !output.contains("(interaction.md)")
            && !output.contains("(revisions.md)")
    );
    assert!(
        output.contains("(#mcp-and-cli-interaction)")
            && output.contains("# MCP and CLI interaction")
    );
    assert!(
        output.contains("(#revisions-findings-and-handoff-references)")
            && output.contains("# Revisions, findings and handoff references")
    );
    for platform_requirement in ["CODEX_HOME", "$nits-review", "mcp__"] {
        assert!(
            !output.contains(platform_requirement),
            "platform-specific prerequisite"
        );
    }
    // Frontmatter and all references survive the documented persistent export.
    let exported = dir.path().join(".claude/skills/nits-review/SKILL.md");
    std::fs::create_dir_all(exported.parent().unwrap()).unwrap();
    std::fs::write(&exported, output.as_bytes()).unwrap();
    assert_eq!(std::fs::read_to_string(exported).unwrap(), output);
}

#[test]
fn help_explains_discovery_and_printing_is_not_installation() {
    let dir = tempfile::tempdir().unwrap();
    let binary = assert_cmd::cargo::cargo_bin("nits");
    command(&binary, dir.path())
        .arg("--help")
        .assert()
        .success()
        .stdout(
            predicate::str::contains("skill").and(predicate::str::contains("no daemon needed")),
        );
    command(&binary, dir.path())
        .args(["skill", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("always Markdown")
                .and(predicate::str::contains("does not install")),
        );
    command(&binary, dir.path())
        .args(["skill", "path"])
        .assert()
        .code(2);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}
