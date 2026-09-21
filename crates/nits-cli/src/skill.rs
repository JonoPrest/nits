//! Portable review instructions embedded from the repository's canonical skill.
//! Cargo flattens the package-local symlink, so published archives contain the
//! same regular files and standalone binaries never read a checkout at runtime.

use std::io::Write as _;

const GUIDE: &str = include_str!("../bundled-skill/SKILL.md");
const INTERACTION: &str = include_str!("../bundled-skill/references/interaction.md");
const REVISIONS: &str = include_str!("../bundled-skill/references/revisions.md");

fn inline_references(text: &str) -> String {
    text.replace("(references/interaction.md)", "(#mcp-and-cli-interaction)")
        .replace("(interaction.md)", "(#mcp-and-cli-interaction)")
        .replace(
            "(references/revisions.md)",
            "(#revisions-findings-and-handoff-references)",
        )
        .replace(
            "(revisions.md)",
            "(#revisions-findings-and-handoff-references)",
        )
}

/// Preserve skill frontmatter for a redirected personal/project `SKILL.md`.
/// All required supporting content follows in the same Markdown document.
fn markdown() -> anyhow::Result<String> {
    let (frontmatter, body) = GUIDE
        .split_once("\n---\n")
        .ok_or_else(|| anyhow::anyhow!("bundled review skill has no frontmatter boundary"))?;
    let provenance = concat!(
        "> Bundled with Nits ",
        env!("CARGO_PKG_VERSION"),
        " from its canonical nits-review skill. These are installed instructions; ",
        "printing them does not inspect a daemon or install/register a skill.\n"
    );
    Ok(inline_references(&format!(
        "{frontmatter}\n---\n\n{provenance}{body}\n{INTERACTION}\n{REVISIONS}"
    )))
}

pub fn print() -> anyhow::Result<()> {
    std::io::stdout().lock().write_all(markdown()?.as_bytes())?;
    Ok(())
}
