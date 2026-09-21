//! Release tasks: work out what a given binary's release needs, and publish
//! the crates it is built from, in dependency order.
//!
//! `cargo xtask release-plan --package nits` prints the plan as JSON (consumed
//! by `.github/workflows/release.yml`); `cargo xtask publish --package nits`
//! carries it out. Both run locally, so a release can be rehearsed off CI.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

use anyhow::{Context, bail};

/// A binary Nits ships to registries. Every release names one of these; adding
/// a target is a variant here and nowhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Releasable {
    /// The `nits` binary — client, daemon (`nits daemon serve`) and MCP
    /// server (`nits mcp`) in one executable.
    Cli,
}

impl Releasable {
    /// The crate name, which is also the crates.io name and the binary name.
    pub fn crate_name(self) -> &'static str {
        match self {
            Self::Cli => "nits",
        }
    }

    /// Every binary this release ships.
    ///
    /// One, deliberately. `nitsd` and `nits-mcp` are libraries linked into
    /// `nits`, which starts its own daemon by re-executing itself
    /// (`nitsd::launch::nits_binary`) — so there is no second executable to
    /// keep in step, in any channel.
    pub fn binaries(self) -> &'static [&'static str] {
        match self {
            Self::Cli => &["nits"],
        }
    }

    /// Parse the `--package` argument. Untrusted input becomes a variant once,
    /// here, so every later match is exhaustive.
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        match s {
            "nits" => Ok(Self::Cli),
            other => bail!("unknown package {other:?}; expected: nits"),
        }
    }
}

/// A workspace member as `cargo metadata` reports it.
#[derive(Debug)]
struct Member {
    name: String,
    version: String,
    /// The crate's directory, relative to the repo root. cargo-generate-rpm
    /// wants a path rather than a package name.
    dir: String,
    /// False for `publish = false` crates, which cargo strips from a published
    /// manifest and which therefore never need publishing themselves.
    publish: bool,
    /// Workspace crates this one depends on, in any kind (normal, build or
    /// dev). Dev-dependencies count: a path dev-dependency that carries a
    /// version stays in the published manifest and has to resolve.
    deps: BTreeSet<String>,
}

/// Read the workspace graph. `--no-deps` keeps this to our own crates.
fn members() -> anyhow::Result<BTreeMap<String, Member>> {
    let out = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .context("running `cargo metadata`")?;
    if !out.status.success() {
        bail!(
            "`cargo metadata` failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let root = meta
        .get("workspace_root")
        .and_then(serde_json::Value::as_str)
        .context("cargo metadata has no `workspace_root`")?
        .to_owned();
    let packages = meta
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .context("cargo metadata has no `packages` array")?;

    let names: BTreeSet<&str> = packages
        .iter()
        .filter_map(|p| p.get("name")?.as_str())
        .collect();

    let mut graph = BTreeMap::new();
    for p in packages {
        let name = p
            .get("name")
            .and_then(serde_json::Value::as_str)
            .context("cargo metadata reported a package without a name")?;
        let version = p
            .get("version")
            .and_then(serde_json::Value::as_str)
            .with_context(|| format!("{name} has no version"))?;
        // `publish` is null when unrestricted, and an array (empty, for
        // `publish = false`) when restricted.
        let publish = match p.get("publish") {
            None | Some(serde_json::Value::Null) => true,
            Some(serde_json::Value::Array(allowed)) => !allowed.is_empty(),
            Some(other) => bail!("{name} has an unexpected `publish` value: {other}"),
        };
        let manifest = p
            .get("manifest_path")
            .and_then(serde_json::Value::as_str)
            .with_context(|| format!("{name} has no manifest_path"))?;
        let dir = std::path::Path::new(manifest)
            .parent()
            .and_then(|d| d.strip_prefix(&root).ok())
            .map(|d| d.display().to_string())
            .with_context(|| format!("{name} is not under the workspace root"))?;
        let deps = p
            .get("dependencies")
            .and_then(serde_json::Value::as_array)
            .map(|ds| {
                ds.iter()
                    .filter_map(|d| d.get("name")?.as_str())
                    .filter(|d| names.contains(d))
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        graph.insert(
            name.to_owned(),
            Member {
                name: name.to_owned(),
                version: version.to_owned(),
                dir,
                publish,
                deps,
            },
        );
    }
    Ok(graph)
}

/// Depth-first post-order over the dependency closure of `root`: a crate is
/// emitted only after everything it depends on, which is exactly the order
/// `cargo publish` needs.
///
/// Edges into `publish = false` crates are not followed. Cargo strips those
/// path dependencies when it packages, so they neither need publishing nor
/// pull anything else in — and not following them is what keeps the legitimate
/// dev-dependency cycles (`nits-client-core` ↔ `nits-test-support`) out of the
/// traversal, leaving the cycle check to catch only real ones.
fn publish_order(graph: &BTreeMap<String, Member>, root: &str) -> anyhow::Result<Vec<String>> {
    let mut order = Vec::new();
    let mut done = BTreeSet::new();
    let mut on_stack = BTreeSet::new();
    visit(graph, root, &mut order, &mut done, &mut on_stack)?;
    Ok(order)
}

fn visit(
    graph: &BTreeMap<String, Member>,
    name: &str,
    order: &mut Vec<String>,
    done: &mut BTreeSet<String>,
    on_stack: &mut BTreeSet<String>,
) -> anyhow::Result<()> {
    if done.contains(name) {
        return Ok(());
    }
    if !on_stack.insert(name.to_owned()) {
        bail!("dependency cycle through {name}");
    }
    let member = graph
        .get(name)
        .with_context(|| format!("{name} is not a workspace member"))?;
    for dep in &member.deps {
        if graph.get(dep).is_some_and(|d| d.publish) {
            visit(graph, dep, order, done, on_stack)?;
        }
    }
    on_stack.remove(name);
    done.insert(name.to_owned());
    if member.publish {
        order.push(member.name.clone());
    }
    Ok(())
}

/// Whether crates.io already serves this exact `name@version`. Shelling out to
/// `curl` keeps xtask free of an HTTP stack; crates.io requires a User-Agent.
fn is_published(name: &str, version: &str) -> anyhow::Result<bool> {
    let url = format!("https://crates.io/api/v1/crates/{name}/{version}");
    let out = Command::new("curl")
        .args([
            "--silent",
            "--location",
            "--user-agent",
            "nits-xtask (https://github.com/JonoPrest/nits)",
            "--output",
            "/dev/null",
            "--write-out",
            "%{http_code}",
            &url,
        ])
        .output()
        .context("running `curl` to query crates.io")?;
    if !out.status.success() {
        bail!("could not reach crates.io for {name}@{version}");
    }
    match String::from_utf8_lossy(&out.stdout).trim() {
        "200" => Ok(true),
        "404" => Ok(false),
        other => bail!("crates.io returned {other} for {name}@{version}"),
    }
}

/// The git tag a release claims. Per-package, so `nits` and `nitsd` version
/// independently.
fn tag_for(package: &str, version: &str) -> String {
    format!("{package}-v{version}")
}

/// Where a release's tag stands relative to the commit being released.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TagState {
    /// No such tag: a fresh release.
    Absent,
    /// The tag exists and points at this commit. A previous run got this far
    /// and something after it failed, so this run is a *resume*, not a
    /// collision — re-running must be able to finish the release.
    AtHead,
    /// The tag exists on a different commit. That is a genuine collision: the
    /// version was released from other code.
    Elsewhere,
}

impl TagState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::AtHead => "at_head",
            Self::Elsewhere => "elsewhere",
        }
    }
}

/// Resolve a revision to a full commit id, or `None` if it does not exist.
/// `repo` is explicit so tests can drive a throwaway repository without
/// changing the process-wide working directory.
fn rev_parse(repo: &std::path::Path, rev: &str) -> anyhow::Result<Option<String>> {
    let out = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", rev])
        .current_dir(repo)
        .output()
        .context("running `git rev-parse`")?;
    let id = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    Ok((!id.is_empty()).then_some(id))
}

/// Whether `tag` exists and, if so, whether it is on the commit being released.
fn tag_state(repo: &std::path::Path, tag: &str) -> anyhow::Result<TagState> {
    // `<tag>^{commit}` peels an annotated tag to the commit it points at.
    let Some(tagged) = rev_parse(repo, &format!("{tag}^{{commit}}"))? else {
        return Ok(TagState::Absent);
    };
    let head = rev_parse(repo, "HEAD^{commit}")?.context("HEAD does not resolve to a commit")?;
    Ok(if tagged == head {
        TagState::AtHead
    } else {
        TagState::Elsewhere
    })
}

/// Print the release plan as JSON. `release.yml` reads `version`, `tag`,
/// `binaries`, `packages` and `collision` from this to decide whether the run
/// may proceed and what it must build.
pub fn plan(package: Releasable) -> anyhow::Result<()> {
    let graph = members()?;
    let root = package.crate_name();
    let member = graph
        .get(root)
        .with_context(|| format!("{root} is not a workspace member"))?;
    let version = member.version.clone();
    let dir = member.dir.clone();
    let tag = tag_for(root, &version);

    let mut crates = Vec::new();
    for name in publish_order(&graph, root)? {
        let member = graph
            .get(&name)
            .with_context(|| format!("{name} vanished from the graph"))?;
        crates.push(serde_json::json!({
            "name": member.name,
            "version": member.version,
            "already_published": is_published(&member.name, &member.version)?,
        }));
    }

    // Every binary this release ships, with the crate directory it is built
    // from, so the deb/rpm job can produce one system package per binary.
    let mut packages = Vec::new();
    for bin in package.binaries() {
        let m = graph
            .get(*bin)
            .with_context(|| format!("no workspace member builds {bin}"))?;
        packages.push(serde_json::json!({ "name": m.name, "dir": m.dir }));
    }

    // What makes a version un-releasable is the *tag*, which is what a binary
    // release claims. Being on crates.io does not: a crate is published
    // whenever it appears in some other package's dependency closure — a
    // `nits` release publishes `nitsd@x` — and that must not then block
    // `nitsd`'s own release of the same version, which has produced no tag,
    // tarball, formula, AUR package, deb or rpm. Re-publishing is separately
    // harmless because `publish()` skips versions crates.io already has.
    let root_published = crates
        .iter()
        .find(|c| c["name"] == root)
        .and_then(|c| c["already_published"].as_bool())
        .unwrap_or(false);

    // A tag on *this* commit means a previous run got partway and something
    // after it failed; that must be resumable, not a collision. Only a tag on
    // different code says the version was already released.
    let tag_state = tag_state(std::path::Path::new("."), &tag)?;
    let collision = tag_state == TagState::Elsewhere;

    let plan = serde_json::json!({
        "package": root,
        "version": version,
        "dir": dir,
        "tag": tag,
        "tag_state": tag_state.as_str(),
        "binaries": package.binaries(),
        "packages": packages,
        "crates": crates,
        "crate_already_published": root_published,
        "collision": collision,
        "collision_reason": if collision {
            format!("tag {tag} already exists on a different commit")
        } else {
            String::new()
        },
    });
    println!("{}", serde_json::to_string_pretty(&plan)?);
    Ok(())
}

/// Publish every crate in the closure that crates.io does not already have,
/// in dependency order. Idempotent: a re-run after a partial failure resumes.
///
/// A dry run cannot verify everything. Cargo resolves a package's dependency
/// versions against the registry index before it will even build the `.crate`,
/// so a crate whose workspace dependencies are not yet published is
/// unverifiable until they are — in a real run they are, one step earlier in
/// this same loop. Those crates are reported as deferred rather than failing
/// the run, which is the difference between a dry run that means something and
/// one that always goes red on a first release.
pub fn publish(package: Releasable, dry_run: bool) -> anyhow::Result<()> {
    let graph = members()?;
    let root = package.crate_name();
    for name in publish_order(&graph, root)? {
        let member = graph
            .get(&name)
            .with_context(|| format!("{name} vanished from the graph"))?;
        if is_published(&member.name, &member.version)? {
            println!(
                "skip {}@{} — already on crates.io",
                member.name, member.version
            );
            continue;
        }
        if dry_run && let Some(blocker) = unpublished_dep(&graph, member)? {
            println!(
                "defer {}@{} — cannot be verified until {blocker} is on crates.io",
                member.name, member.version
            );
            continue;
        }
        println!("publish {}@{}", member.name, member.version);
        publish_one(&member.name, dry_run)?;
    }
    Ok(())
}

/// How many times to wait out a crates.io rate limit before giving up, and how
/// long to wait between attempts. The new-crate limit refills roughly every ten
/// minutes, so this covers it with margin without waiting forever on a limit
/// that is never going to clear.
const RATE_LIMIT_ATTEMPTS: u32 = 20;
const RATE_LIMIT_WAIT: std::time::Duration = std::time::Duration::from_mins(1);

/// `cargo publish` one crate, waiting out a crates.io rate limit rather than
/// failing on it.
///
/// crates.io allows a small burst of *new* crates and then one per interval, so
/// a first release of several new crates gets partway and is refused with 429.
/// Nothing is wrong with the package at that point — the only correct response
/// is to wait — but treating it as fatal means a human re-running the job once
/// per interval.
fn publish_one(name: &str, dry_run: bool) -> anyhow::Result<()> {
    for attempt in 1..=RATE_LIMIT_ATTEMPTS {
        let mut cmd = Command::new("cargo");
        cmd.args(["publish", "--package", name, "--locked"]);
        if dry_run {
            cmd.arg("--dry-run");
        }
        let out = cmd
            .output()
            .with_context(|| format!("running `cargo publish -p {name}`"))?;

        // cargo writes progress to stderr; relay both so the job log still
        // reads like a normal publish.
        print!("{}", String::from_utf8_lossy(&out.stdout));
        eprint!("{}", String::from_utf8_lossy(&out.stderr));
        if out.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&out.stderr);
        let Some(after) = rate_limited_until(&stderr) else {
            bail!("`cargo publish -p {name}` failed");
        };
        if attempt == RATE_LIMIT_ATTEMPTS {
            bail!(
                "`cargo publish -p {name}` is still rate limited after {attempt} attempts; \
                 crates.io last said to try again after {after}"
            );
        }
        println!(
            "rate limited on {name}: crates.io says try again after {after} \
             — waiting {}s (attempt {attempt}/{RATE_LIMIT_ATTEMPTS})",
            RATE_LIMIT_WAIT.as_secs()
        );
        std::thread::sleep(RATE_LIMIT_WAIT);
    }
    Ok(())
}

/// The time crates.io told us to retry after, if this failure was a rate limit.
///
/// The message is: `the remote server responded with an error (status 429 Too
/// Many Requests): You have published too many new crates in a short period of
/// time. Please try again after Wed, 02 Sep 2026 11:46:21 GMT and see …`. We
/// only report the timestamp — parsing it to sleep exactly that long would need
/// a date library for no benefit over polling.
fn rate_limited_until(stderr: &str) -> Option<&str> {
    if !stderr.contains("429") {
        return None;
    }
    let after = stderr.split("Please try again after ").nth(1)?;
    Some(after.split(" and see").next().unwrap_or(after).trim())
}

/// The first publishable workspace dependency of `member` that crates.io does
/// not yet have, if any.
fn unpublished_dep(
    graph: &BTreeMap<String, Member>,
    member: &Member,
) -> anyhow::Result<Option<String>> {
    for dep in &member.deps {
        let Some(dep) = graph.get(dep).filter(|d| d.publish) else {
            continue;
        };
        if !is_published(&dep.name, &dep.version)? {
            return Ok(Some(dep.name.clone()));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every releasable package, so these tests cover whatever the enum
    /// grows to rather than a hand-kept list per test.
    const ALL: &[Releasable] = &[Releasable::Cli];

    fn member(name: &str, deps: &[&str], publish: bool) -> (String, Member) {
        (
            name.to_owned(),
            Member {
                name: name.to_owned(),
                version: "0.1.0".to_owned(),
                dir: format!("crates/{name}"),
                publish,
                deps: deps.iter().map(|d| (*d).to_owned()).collect(),
            },
        )
    }

    #[test]
    fn every_releasable_name_round_trips() {
        for &r in ALL {
            assert_eq!(Releasable::parse(r.crate_name()).expect("parses"), r);
        }
    }

    #[test]
    fn a_release_ships_exactly_its_own_binary() {
        // The daemon and the MCP server are subcommands of `nits`, not
        // executables of their own: a release that shipped a second binary
        // would be one more thing to install and to keep in version step.
        for &r in ALL {
            assert_eq!(r.binaries(), [r.crate_name()]);
        }
    }

    #[test]
    fn every_shipped_binary_is_built_by_a_known_package() {
        let packages: BTreeSet<&str> = ALL.iter().map(|r| r.crate_name()).collect();
        for r in ALL {
            for bin in r.binaries() {
                assert!(packages.contains(bin), "no package builds {bin}");
            }
        }
    }

    #[test]
    fn unknown_package_is_rejected() {
        // Libraries `nits` links, not binaries anyone releases on their own.
        assert!(Releasable::parse("nitsd").is_err());
        assert!(Releasable::parse("nits-mcp").is_err());
        assert!(Releasable::parse("moor").is_err());
    }

    #[test]
    fn dependencies_are_published_before_their_dependents() {
        let graph: BTreeMap<_, _> = [
            member("nits", &["nitsd", "nits-protocol"], true),
            member("nitsd", &["nits-protocol"], true),
            member("nits-protocol", &[], true),
        ]
        .into_iter()
        .collect();

        let order = publish_order(&graph, "nits").expect("orders");
        assert_eq!(order, ["nits-protocol", "nitsd", "nits"]);
    }

    #[test]
    fn unpublishable_crates_are_not_published_and_pull_in_nothing() {
        // Cargo strips a `publish = false` path dependency, so neither it nor
        // anything only it needs has to be on crates.io.
        let graph: BTreeMap<_, _> = [
            member("nits", &["nits-test-support", "nits-protocol"], true),
            member("nits-test-support", &["some-private-only-dep"], false),
            member("some-private-only-dep", &[], true),
            member("nits-protocol", &[], true),
        ]
        .into_iter()
        .collect();

        let order = publish_order(&graph, "nits").expect("orders");
        assert_eq!(order, ["nits-protocol", "nits"]);
    }

    #[test]
    fn a_dev_dependency_cycle_through_a_private_crate_is_not_a_cycle() {
        // The real shape: nits-client-core dev-depends on nits-test-support,
        // which depends back on nits-client-core. Cargo strips the edge.
        let graph: BTreeMap<_, _> = [
            member("nits-client-core", &["nits-test-support"], true),
            member("nits-test-support", &["nits-client-core"], false),
        ]
        .into_iter()
        .collect();

        let order = publish_order(&graph, "nits-client-core").expect("orders");
        assert_eq!(order, ["nits-client-core"]);
    }

    #[test]
    fn a_cycle_is_an_error_not_a_hang() {
        let graph: BTreeMap<_, _> = [member("a", &["b"], true), member("b", &["a"], true)]
            .into_iter()
            .collect();

        assert!(publish_order(&graph, "a").is_err());
    }

    /// `tag_state` shells out to git, so this drives a throwaway repository
    /// rather than mocking it — the same rule the rest of the workspace follows.
    fn git(dir: &std::path::Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .status()
            .expect("git runs")
            .success();
        assert!(ok, "git {args:?} failed");
    }

    #[test]
    fn a_tag_on_this_commit_is_resumable_and_one_elsewhere_is_a_collision() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a"), "1").expect("write");
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", "one"]);

        assert_eq!(tag_state(dir, "v1").expect("state"), TagState::Absent);

        git(dir, &["tag", "-a", "v1", "-m", "v1"]);
        // A previous run tagged this very commit and then failed: resume.
        assert_eq!(tag_state(dir, "v1").expect("state"), TagState::AtHead);

        std::fs::write(dir.join("a"), "2").expect("write");
        git(dir, &["commit", "-qam", "two"]);
        // The tag now names other code: a real collision.
        assert_eq!(tag_state(dir, "v1").expect("state"), TagState::Elsewhere);
    }

    #[test]
    fn a_rate_limit_is_recognised_and_its_retry_time_extracted() {
        let stderr = "\
error: failed to publish nits-client-host v0.1.0 to registry at https://crates.io
  the remote server responded with an error (status 429 Too Many Requests): You \
have published too many new crates in a short period of time. Please try again \
after Wed, 02 Sep 2026 11:46:21 GMT and see https://crates.io/docs/rate-limits \
for more details.";
        assert_eq!(
            rate_limited_until(stderr),
            Some("Wed, 02 Sep 2026 11:46:21 GMT")
        );
    }

    #[test]
    fn other_failures_are_not_treated_as_rate_limits() {
        // A verified email address is required — retrying this forever would
        // turn a clear error into a twenty-minute hang.
        let stderr = "the remote server responded with an error (status 400 Bad \
Request): A verified email address is required to publish crates to crates.io.";
        assert_eq!(rate_limited_until(stderr), None);
        assert_eq!(rate_limited_until("some unrelated build failure"), None);
    }

    #[test]
    fn tags_are_namespaced_per_package() {
        assert_eq!(tag_for("nits", "0.2.0"), "nits-v0.2.0");
        assert_eq!(tag_for("nitsd", "0.2.0"), "nitsd-v0.2.0");
    }
}
