//! Keyboard model (ARCHITECTURE §6.4): a data table mapping
//! `(Context, KeySeq) → Command`, a chord/sequence parser, and the hint and
//! help views generated from the table. The UI captures keys and sends
//! chords; every behaviour lives here so it is identical across hosts.
//!
//! A [`Command`] is what a key *means* ("next file"); the focus model
//! (`focus.rs`) turns it into an [`crate::Action`] with concrete targets.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use strum::{Display, EnumIter, EnumString, IntoEnumIterator};

/// Where the user's focus is; decides which bindings apply.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, EnumIter, EnumString, Display,
)]
pub enum Context {
    Global,
    ReviewList,
    Tree,
    Diff,
    Thread,
    Requests,
    Composer,
    CommitStepper,
    Help,
}

/// Vim-style editing mode (UI-DESIGN: fully modal). `Normal` is keys as
/// commands; `Insert` is any text editor (the composer, search boxes) —
/// only `esc` and `ctrl+enter` stay chords there. `Visual` is line
/// selection on the diff (`V`): motions extend it, `c` comments on it.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    EnumIter,
    EnumString,
    Display,
    Default,
    PartialOrd,
    Ord,
)]
pub enum Mode {
    /// Accepts the config file's lowercase table names; serializes
    /// CamelCase like every other enum on the UI boundary.
    #[default]
    #[serde(alias = "normal")]
    Normal,
    #[serde(alias = "insert")]
    Insert,
    #[serde(alias = "visual")]
    Visual,
}

/// What a key means. Unit-only: targets come from the focus at resolution.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, EnumIter, EnumString, Display,
)]
pub enum Command {
    MoveDown,
    MoveUp,
    PageDown,
    PageUp,
    GoTop,
    GoBottom,
    NextHunk,
    PrevHunk,
    NextFile,
    PrevFile,
    NextComment,
    PrevComment,
    /// Open what is focused: a review, a file, a thread's location.
    Open,
    /// Leave the current level: close help, discard a draft, close the
    /// file, close the review.
    Back,
    /// Cycle focus between the panels.
    NextPanel,
    ToggleViewed,
    Comment,
    ReviewFinding,
    InformationalNote,
    Reply,
    /// Delete the focused thread's root comment (own comments only).
    Delete,
    /// Apply the focused suggestion thread's patch to the working tree.
    ApplySuggestion,
    ToggleResolved,
    DeferFinding,
    FileSearch,
    ToggleLayout,
    ToggleWhitespace,
    ToggleHelp,
    /// Show the "Files changed" tab.
    TabFiles,
    /// Show the "Conversation" tab.
    TabConversation,
    /// Show the "Browse" tab.
    TabBrowse,
    ToggleSidebar,
    /// Submit the open composer. The editor lives in the host, which
    /// handles the chord itself; the binding exists so hints, help and
    /// tooltips can derive it.
    Submit,
    Connect,
    Disconnect,
    /// Fetch the commit list for the focused repo.
    Commits,
    /// Scope: all changes (`base → head` as resolved).
    ScopeAll,
    /// Scope: step commit by commit (worktree last).
    ScopeByCommit,
    /// Toggle the `+ working tree` part of the all-changes scope.
    ScopeWorktree,
    /// Re-render the focused file with more context (UI-DESIGN
    /// §expanders).
    ExpandContext,
    /// Open the content-search palette (UI-DESIGN §Search, `F`).
    ContentSearch,
    /// Open the actions palette (`:`): every command by name.
    ActionPalette,
    /// Copy the focused file's repo-relative path to the clipboard (the
    /// shell performs the copy; the core records the intent).
    CopyPath,
    CopyCheckout,
    NewReview,
    GoHome,
    /// Copy the focused thread or selected reply's portable reference.
    CopyReference,
    NextReply,
    PrevReply,
    /// Collapse the focused tree node's parent dir (neo-tree `C`); on an
    /// open dir, the dir itself. Focus follows.
    CollapseParent,
    /// Collapse every dir of the tree.
    CollapseAll,
    /// Fold/unfold the focused file's section in the stacked diff.
    ToggleFileCollapse,
    /// Re-render the focused file with the whole file as context.
    ExpandFile,
    /// Focus the file tree panel.
    FocusTree,
    /// Focus the open diff.
    FocusDiff,
    /// Focus the thread list.
    FocusThreads,
    FocusRequests,
    CheckCurrent,
    CheckRequested,
    CheckpointDelta,
    /// Focus the commits list.
    FocusCommits,
    /// Re-list workspaces and reviews.
    Refresh,
    /// Enter (or leave) Visual line selection on the diff (`V`).
    VisualMode,
    /// Expand context upward from the focused row (`z u`; whole-file
    /// re-render until band splicing lands).
    ExpandUp,
    /// Expand context downward from the focused row (`z d`).
    ExpandDown,
    /// Comment on the open file (`z c`, file-level anchor).
    CommentOnFile,
    /// Target the removed (left) half of the focused row (`h`/Left).
    SideBase,
    /// Target the added (right) half of the focused row (`l`/Right).
    SideHead,
    /// Put the focused row in the middle of the viewport (`z z`).
    CenterView,
    /// Put the focused row at the top of the viewport (`z t`).
    ViewTop,
    /// Put the focused row at the bottom of the viewport (`z b`).
    ViewBottom,
}

/// A named key that is not a character.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, EnumIter, EnumString, Display,
)]
#[strum(serialize_all = "lowercase")]
pub enum NamedKey {
    Enter,
    Esc,
    Tab,
    Backspace,
    Space,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, strum::EnumDiscriminants,
)]
#[strum_discriminants(name(KeyCodeKind), derive(Hash, EnumIter))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum KeyCode {
    Char { c: char },
    Named { key: NamedKey },
}

/// Modifier keys held with a key. Shift is implied by an upper-case
/// `Char` and only meaningful on named keys.
// Four independent flags is the domain; a bit set would hide the names.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// Command on macOS, Super/Windows elsewhere.
    pub meta: bool,
}

/// One key press with its modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyChord {
    pub key: KeyCode,
    pub mods: Modifiers,
}

impl KeyChord {
    #[must_use]
    pub const fn char(c: char) -> Self {
        Self {
            key: KeyCode::Char { c },
            mods: Modifiers {
                ctrl: false,
                alt: false,
                shift: false,
                meta: false,
            },
        }
    }

    #[must_use]
    pub const fn named(key: NamedKey) -> Self {
        Self {
            key: KeyCode::Named { key },
            mods: Modifiers {
                ctrl: false,
                alt: false,
                shift: false,
                meta: false,
            },
        }
    }
}

/// Why a chord or sequence text could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyParseError {
    #[error("empty key sequence")]
    Empty,
    #[error("unknown modifier {0:?}")]
    UnknownModifier(String),
    #[error("unknown key {0:?}")]
    UnknownKey(String),
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.mods.ctrl {
            f.write_str("ctrl+")?;
        }
        if self.mods.alt {
            f.write_str("alt+")?;
        }
        if self.mods.shift {
            f.write_str("shift+")?;
        }
        if self.mods.meta {
            f.write_str("meta+")?;
        }
        match self.key {
            KeyCode::Char { c: ' ' } => f.write_str("space"),
            KeyCode::Char { c } => write!(f, "{c}"),
            KeyCode::Named { key } => write!(f, "{key}"),
        }
    }
}

impl FromStr for KeyChord {
    type Err = KeyParseError;

    /// `ctrl+p`, `shift+enter`, `?`, `g`, `space`. Modifiers are
    /// case-insensitive; a single character is taken as is.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(KeyParseError::Empty);
        }
        let mut mods = Modifiers::default();
        let mut parts: Vec<&str> = s.split('+').collect();
        // A literal '+' key: "ctrl++" splits into ["ctrl", "", ""].
        let last = if parts.len() >= 2 && parts[parts.len() - 1].is_empty() {
            parts.pop();
            parts.pop();
            "+"
        } else {
            parts.pop().unwrap_or_default()
        };
        for m in parts {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "c" => mods.ctrl = true,
                "alt" | "option" | "a" => mods.alt = true,
                "shift" | "s" => mods.shift = true,
                "meta" | "cmd" | "super" | "win" | "m" => mods.meta = true,
                other => return Err(KeyParseError::UnknownModifier(other.to_owned())),
            }
        }
        let mut chars = last.chars();
        let key = match (chars.next(), chars.next()) {
            (Some(c), None) => KeyCode::Char { c },
            (Some(_), Some(_)) => KeyCode::Named {
                key: NamedKey::from_str(&last.to_ascii_lowercase())
                    .map_err(|_| KeyParseError::UnknownKey(last.to_owned()))?,
            },
            (None, _) => return Err(KeyParseError::Empty),
        };
        Ok(Self { key, mods })
    }
}

/// One or more chords pressed in order (`g g`, `] c`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeySeq(Vec<KeyChord>);

impl KeySeq {
    #[must_use]
    pub fn single(chord: KeyChord) -> Self {
        Self(vec![chord])
    }

    pub fn new(chords: Vec<KeyChord>) -> Result<Self, KeyParseError> {
        if chords.is_empty() {
            Err(KeyParseError::Empty)
        } else {
            Ok(Self(chords))
        }
    }

    #[must_use]
    pub fn chords(&self) -> &[KeyChord] {
        &self.0
    }

    #[must_use]
    pub fn starts_with(&self, prefix: &[KeyChord]) -> bool {
        self.0.starts_with(prefix)
    }
}

impl fmt::Display for KeySeq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, c) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{c}")?;
        }
        Ok(())
    }
}

impl FromStr for KeySeq {
    type Err = KeyParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let chords = s
            .split_whitespace()
            .map(KeyChord::from_str)
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(chords)
    }
}

impl Serialize for KeySeq {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for KeySeq {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// One row of the table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub context: Context,
    pub keys: KeySeq,
    pub command: Command,
    /// Shown in the hint bar for its context.
    pub primary: bool,
}

/// A user override: same shape as a binding; replaces every default
/// binding of `command` in `context`. `keys: None` unbinds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Override {
    pub context: Context,
    pub command: Command,
    pub keys: Option<KeySeq>,
    #[serde(default)]
    pub primary: bool,
}

/// The override file: what the host stores under [`Keymap::KEY`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Overrides {
    pub bindings: Vec<Override>,
}

/// Two bindings that cannot both fire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Conflict {
    pub context: Context,
    pub keys: KeySeq,
    pub commands: Vec<Command>,
}

/// The full table plus which rows came from overrides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    bindings: Vec<Binding>,
    overridden: Vec<(Context, Command)>,
    /// The leader chord (`<leader>` in config sequences). Default: space.
    leader: KeyChord,
    /// Which-key labels: a pending prefix → its group name.
    groups: Vec<(KeySeq, String)>,
}

/// Why a keys config was rejected. Collisions are NOT errors — they are
/// reported (`conflicts`), vim-style; only unparsable input rejects.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeysError {
    #[error("{0:?} is not an action (see `nits keys init` for the list)")]
    UnknownAction(String),
    #[error("{action}: {source}")]
    BadKeys {
        action: String,
        source: KeyParseError,
    },
    #[error("leader: {0}")]
    BadLeader(KeyParseError),
    #[error("{action} cannot be bound in {mode} mode (valid: {valid})")]
    WrongMode {
        action: String,
        mode: Mode,
        valid: String,
    },
    #[error("group prefix {prefix:?}: {source}")]
    BadGroup {
        prefix: String,
        source: KeyParseError,
    },
}

/// The typed keys config (UI-DESIGN: modal keys): what `keys.toml`
/// deserializes to and the host stores under [`Keymap::KEY`]. Action
/// names are `snake_case` command names; sequences may use `<leader>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct KeysConfig {
    /// The leader chord; default `space`.
    #[serde(default)]
    pub leader: Option<String>,
    /// `mode → action → sequences`. An action listed replaces its default
    /// bindings in that mode; `[]` unbinds it.
    #[serde(default)]
    pub bindings: std::collections::BTreeMap<Mode, std::collections::BTreeMap<String, Vec<String>>>,
    /// Which-key labels: `prefix sequence → group label`.
    #[serde(default)]
    pub groups: std::collections::BTreeMap<String, String>,
}

/// Parse a sequence that may contain `<leader>` tokens.
pub fn resolve_seq(text: &str, leader: KeyChord) -> Result<KeySeq, KeyParseError> {
    let chords = text
        .split_whitespace()
        .map(|tok| {
            if tok.eq_ignore_ascii_case("<leader>") {
                Ok(leader)
            } else {
                KeyChord::from_str(tok)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    KeySeq::new(chords)
}

/// How a chord sequence matched the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookup {
    /// Exactly one binding matched.
    Command(Command),
    /// Some binding starts with the sequence; wait for more.
    Prefix,
    None,
}

/// `keys!("] c")` parses a sequence literal at table-build time. Only used
/// on literals in this module, so the parse cannot fail; a bad literal
/// shows up in `default_table_parses`.
macro_rules! keys {
    ($s:literal) => {
        $s.parse::<KeySeq>()
            .unwrap_or_else(|_| KeySeq::single(KeyChord::char('\0')))
    };
}

impl Keymap {
    /// Host KV key the overrides live under.
    pub const KEY: &'static str = "nits/keymap";

    /// The built-in table with the default leader (space).
    #[must_use]
    pub fn default_table() -> Self {
        Self::default_table_with(KeyChord::named(NamedKey::Space))
    }

    /// The built-in table (§6.4, UI-DESIGN: modal keys). Vim-style
    /// movement, a `g` goto group, and a `<leader>` tree.
    // One line per binding; splitting would hide the whole table.
    #[allow(clippy::too_many_lines)]
    #[must_use]
    pub fn default_table_with(leader: KeyChord) -> Self {
        use Command as C;
        use Context as X;
        // A leader-group binding: `<leader> <c>`.
        let l = |c: char, command: Command| Binding {
            context: X::Global,
            keys: KeySeq::new(vec![leader, KeyChord::char(c)])
                .unwrap_or_else(|_| KeySeq::single(leader)),
            command,
            primary: false,
        };
        let b = |context: Context, keys: KeySeq, command: Command, primary: bool| Binding {
            context,
            keys,
            command,
            primary,
        };
        let bindings = vec![
            // Global
            b(X::Global, keys!("?"), C::ToggleHelp, true),
            b(X::Global, keys!("tab"), C::NextPanel, false),
            b(X::Global, keys!("t"), C::FileSearch, true),
            b(X::Global, keys!("ctrl+p"), C::FileSearch, false),
            b(X::Global, keys!("meta+p"), C::FileSearch, false),
            b(X::Global, keys!("F"), C::ContentSearch, false),
            b(X::Global, keys!(":"), C::ActionPalette, false),
            b(X::Global, keys!("1"), C::TabFiles, false),
            b(X::Global, keys!("2"), C::TabConversation, false),
            b(X::Global, keys!("3"), C::TabBrowse, false),
            b(X::Global, keys!("s"), C::ToggleLayout, false),
            b(X::Global, keys!("w"), C::ToggleWhitespace, false),
            // `g` is vim's goto group (which-key label "Go"); the
            // configurable `<leader>` (default space) holds the rest.
            b(X::Global, keys!("g f"), C::NextFile, false),
            b(X::Global, keys!("g F"), C::PrevFile, false),
            // goto a panel: e(xplorer)/d(iff)/t(hreads)/m (commits).
            b(X::Global, keys!("g e"), C::FocusTree, false),
            b(X::Global, keys!("g d"), C::FocusDiff, false),
            b(X::Global, keys!("g t"), C::FocusThreads, false),
            b(X::Global, keys!("g r"), C::FocusRequests, false),
            b(X::Global, keys!("g m"), C::FocusCommits, false),
            l('a', C::ScopeAll),
            l('c', C::ScopeByCommit),
            l('w', C::ScopeWorktree),
            l('s', C::ToggleLayout),
            l('h', C::ToggleWhitespace),
            l('b', C::ToggleSidebar),
            l('C', C::Commits),
            l('i', C::InformationalNote),
            l('k', C::CheckCurrent),
            b(X::Requests, keys!("v"), C::CheckRequested, false),
            l('d', C::CheckpointDelta),
            l('f', C::ReviewFinding),
            b(X::Global, keys!("esc"), C::Back, false),
            b(X::Global, keys!("ctrl+shift+c"), C::Connect, false),
            b(X::Global, keys!("ctrl+shift+d"), C::Disconnect, false),
            b(X::Global, keys!("g W"), C::GoHome, false),
            // Review list
            b(X::ReviewList, keys!("j"), C::MoveDown, true),
            b(X::ReviewList, keys!("k"), C::MoveUp, true),
            b(X::ReviewList, keys!("down"), C::MoveDown, false),
            b(X::ReviewList, keys!("up"), C::MoveUp, false),
            b(X::ReviewList, keys!("g g"), C::GoTop, false),
            b(X::ReviewList, keys!("G"), C::GoBottom, false),
            b(X::ReviewList, keys!("enter"), C::Open, true),
            b(X::ReviewList, keys!("R"), C::Refresh, false),
            b(X::ReviewList, keys!("y"), C::CopyCheckout, true),
            b(X::ReviewList, keys!("N"), C::NewReview, true),
            // Tree
            b(X::Tree, keys!("j"), C::MoveDown, true),
            b(X::Tree, keys!("k"), C::MoveUp, true),
            b(X::Tree, keys!("down"), C::MoveDown, false),
            b(X::Tree, keys!("up"), C::MoveUp, false),
            b(X::Tree, keys!("g g"), C::GoTop, false),
            b(X::Tree, keys!("G"), C::GoBottom, false),
            b(X::Tree, keys!("enter"), C::Open, true),
            b(X::Tree, keys!("v"), C::ToggleViewed, true),
            b(X::Tree, keys!("] f"), C::NextFile, false),
            b(X::Tree, keys!("[ f"), C::PrevFile, false),
            b(X::Tree, keys!("n"), C::NextHunk, false),
            b(X::Tree, keys!("p"), C::PrevHunk, false),
            b(X::Tree, keys!("c"), C::Comment, true),
            b(X::Tree, keys!("y"), C::CopyPath, false),
            b(X::Tree, keys!("C"), C::CollapseParent, false),
            b(X::Tree, keys!("z"), C::CollapseAll, false),
            // Diff
            b(X::Diff, keys!("j"), C::MoveDown, true),
            b(X::Diff, keys!("k"), C::MoveUp, true),
            b(X::Diff, keys!("down"), C::MoveDown, false),
            b(X::Diff, keys!("up"), C::MoveUp, false),
            b(X::Diff, keys!("ctrl+d"), C::PageDown, false),
            b(X::Diff, keys!("ctrl+u"), C::PageUp, false),
            b(X::Diff, keys!("pagedown"), C::PageDown, false),
            b(X::Diff, keys!("pageup"), C::PageUp, false),
            b(X::Diff, keys!("g g"), C::GoTop, false),
            b(X::Diff, keys!("G"), C::GoBottom, false),
            b(X::Diff, keys!("n"), C::NextHunk, true),
            b(X::Diff, keys!("p"), C::PrevHunk, true),
            b(X::Diff, keys!("] f"), C::NextFile, true),
            b(X::Diff, keys!("[ f"), C::PrevFile, false),
            b(X::Diff, keys!("] c"), C::NextComment, false),
            b(X::Diff, keys!("[ c"), C::PrevComment, false),
            b(X::Diff, keys!("c"), C::Comment, true),
            b(X::Diff, keys!("v"), C::ToggleViewed, true),
            b(X::Diff, keys!("y"), C::CopyPath, false),
            b(X::Diff, keys!("x"), C::ExpandContext, false),
            b(X::Diff, keys!("C"), C::ToggleFileCollapse, false),
            b(X::Diff, keys!("X"), C::ExpandFile, false),
            b(X::Diff, keys!("V"), C::VisualMode, false),
            // `z` is the expand and scroll group (which-key "Expand/scroll").
            b(X::Diff, keys!("z u"), C::ExpandUp, false),
            b(X::Diff, keys!("z d"), C::ExpandDown, false),
            b(X::Diff, keys!("z c"), C::CommentOnFile, false),
            // A modified row is two commentable cells: h/l are canonical;
            // the arrows are non-primary aliases of the same commands.
            b(X::Diff, keys!("h"), C::SideBase, false),
            b(X::Diff, keys!("l"), C::SideHead, false),
            b(X::Diff, keys!("left"), C::SideBase, false),
            b(X::Diff, keys!("right"), C::SideHead, false),
            // The scroll family moves the view, not the cursor.
            b(X::Diff, keys!("z z"), C::CenterView, false),
            b(X::Diff, keys!("z t"), C::ViewTop, false),
            b(X::Diff, keys!("z b"), C::ViewBottom, false),
            b(X::Diff, keys!("enter"), C::Open, false),
            // Review requests
            b(X::Requests, keys!("j"), C::MoveDown, true),
            b(X::Requests, keys!("k"), C::MoveUp, true),
            b(X::Requests, keys!("down"), C::MoveDown, false),
            b(X::Requests, keys!("up"), C::MoveUp, false),
            b(X::Requests, keys!("g g"), C::GoTop, false),
            b(X::Requests, keys!("G"), C::GoBottom, false),
            b(X::Requests, keys!("enter"), C::Open, true),
            // Thread
            b(X::Thread, keys!("y"), C::CopyReference, true),
            b(X::Thread, keys!("]"), C::NextReply, false),
            b(X::Thread, keys!("["), C::PrevReply, false),
            b(X::Thread, keys!("j"), C::MoveDown, true),
            b(X::Thread, keys!("k"), C::MoveUp, true),
            b(X::Thread, keys!("down"), C::MoveDown, false),
            b(X::Thread, keys!("up"), C::MoveUp, false),
            b(X::Thread, keys!("g g"), C::GoTop, false),
            b(X::Thread, keys!("G"), C::GoBottom, false),
            b(X::Thread, keys!("enter"), C::Open, true),
            b(X::Thread, keys!("r"), C::Reply, true),
            b(X::Thread, keys!("x"), C::ToggleResolved, true),
            b(X::Thread, keys!("D"), C::DeferFinding, true),
            b(X::Thread, keys!("d"), C::Delete, false),
            b(X::Thread, keys!("a"), C::ApplySuggestion, false),
            // Composer: everything else is text; the host submits.
            b(X::Composer, keys!("ctrl+enter"), C::Submit, true),
            b(X::Composer, keys!("esc"), C::Back, true),
            // Commit stepper
            b(X::CommitStepper, keys!("j"), C::MoveDown, true),
            b(X::CommitStepper, keys!("k"), C::MoveUp, true),
            b(X::CommitStepper, keys!("down"), C::MoveDown, false),
            b(X::CommitStepper, keys!("up"), C::MoveUp, false),
            b(X::CommitStepper, keys!("n"), C::NextHunk, false),
            b(X::CommitStepper, keys!("p"), C::PrevHunk, false),
            b(X::CommitStepper, keys!("enter"), C::Open, true),
            b(X::CommitStepper, keys!("g g"), C::GoTop, false),
            b(X::CommitStepper, keys!("G"), C::GoBottom, false),
            // Help
            b(X::Help, keys!("j"), C::MoveDown, false),
            b(X::Help, keys!("k"), C::MoveUp, false),
            b(X::Help, keys!("?"), C::ToggleHelp, true),
            b(X::Help, keys!("esc"), C::Back, true),
        ];
        Self {
            bindings,
            overridden: Vec::new(),
            leader,
            groups: vec![
                (KeySeq::single(leader), "Leader".to_owned()),
                (keys!("g"), "Goto".to_owned()),
                (keys!("z"), "Expand/scroll".to_owned()),
            ],
        }
    }

    /// The leader chord (`<leader>` in config sequences).
    #[must_use]
    pub fn leader(&self) -> KeyChord {
        self.leader
    }

    /// Which-key label for a pending prefix, when one is configured.
    #[must_use]
    pub fn pending_label(&self, pressed: &[KeyChord]) -> Option<String> {
        self.groups
            .iter()
            .find(|(k, _)| k.chords() == pressed)
            .map(|(_, label)| label.clone())
    }

    /// The default table with a [`KeysConfig`] applied: the leader swaps,
    /// listed actions replace their default bindings in that mode (`[]`
    /// unbinds), groups add which-key labels. Collisions do not reject —
    /// they surface via [`Keymap::conflicts`].
    pub fn with_config(config: &KeysConfig) -> Result<Self, KeysError> {
        let leader = match &config.leader {
            Some(text) => resolve_seq(text, KeyChord::named(NamedKey::Space))
                .map_err(KeysError::BadLeader)
                .and_then(|seq| match seq.chords() {
                    [one] => Ok(*one),
                    _ => Err(KeysError::BadLeader(KeyParseError::Empty)),
                })?,
            None => KeyChord::named(NamedKey::Space),
        };
        let mut map = Self::default_table_with(leader);
        for (mode, actions) in &config.bindings {
            for (name, seqs) in actions {
                let command =
                    command_named(name).ok_or_else(|| KeysError::UnknownAction(name.clone()))?;
                if !modes_of(command).contains(mode) {
                    return Err(KeysError::WrongMode {
                        action: name.clone(),
                        mode: *mode,
                        valid: modes_of(command)
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", "),
                    });
                }
                // The contexts this mode's bindings live in. Visual reuses
                // the Diff-context rows (the mode is derived at resolution,
                // not stored on bindings).
                let in_mode = |ctx: Context| match mode {
                    Mode::Insert => ctx == Context::Composer,
                    Mode::Normal => ctx != Context::Composer,
                    Mode::Visual => ctx == Context::Diff,
                };
                let mut contexts: Vec<(Context, bool)> = map
                    .bindings
                    .iter()
                    .filter(|b| b.command == command && in_mode(b.context))
                    .map(|b| (b.context, b.primary))
                    .collect();
                contexts.dedup_by_key(|(c, _)| *c);
                if contexts.is_empty() {
                    contexts.push((
                        if *mode == Mode::Insert {
                            Context::Composer
                        } else {
                            Context::Global
                        },
                        false,
                    ));
                }
                map.bindings
                    .retain(|b| !(b.command == command && in_mode(b.context)));
                for (i, seq_text) in seqs.iter().enumerate() {
                    let keys =
                        resolve_seq(seq_text, leader).map_err(|source| KeysError::BadKeys {
                            action: name.clone(),
                            source,
                        })?;
                    for (context, primary) in &contexts {
                        map.bindings.push(Binding {
                            context: *context,
                            keys: keys.clone(),
                            command,
                            // Only the first sequence carries the hint-bar
                            // slot; the rest are aliases (`?` shows them).
                            primary: *primary && i == 0,
                        });
                    }
                }
                for (context, _) in &contexts {
                    map.overridden.push((*context, command));
                }
            }
        }
        for (prefix, label) in &config.groups {
            let seq = resolve_seq(prefix, leader).map_err(|source| KeysError::BadGroup {
                prefix: prefix.clone(),
                source,
            })?;
            map.groups.retain(|(k, _)| *k != seq);
            map.groups.push((seq, label.clone()));
        }
        Ok(map)
    }

    /// The default table with `overrides` applied.
    #[must_use]
    pub fn with_overrides(overrides: &Overrides) -> Self {
        let mut map = Self::default_table();
        for o in &overrides.bindings {
            map.bindings
                .retain(|b| !(b.context == o.context && b.command == o.command));
            if let Some(keys) = &o.keys {
                map.bindings.push(Binding {
                    context: o.context,
                    keys: keys.clone(),
                    command: o.command,
                    primary: o.primary,
                });
            }
            map.overridden.push((o.context, o.command));
        }
        map
    }

    #[must_use]
    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    #[must_use]
    pub fn is_overridden(&self, context: Context, command: Command) -> bool {
        self.overridden.contains(&(context, command))
    }

    /// Bindings that apply in `context`: its own, then Global's.
    pub fn applicable(&self, context: Context) -> impl Iterator<Item = &Binding> {
        self.bindings.iter().filter(move |b| {
            b.context == context || (b.context == Context::Global && context != Context::Global)
        })
    }

    /// Match `pressed` against the bindings for `context`. A binding of the
    /// context itself shadows a Global one with the same keys.
    #[must_use]
    pub fn lookup(&self, context: Context, pressed: &[KeyChord]) -> Lookup {
        let exact = |ctx: Context| {
            self.bindings
                .iter()
                .find(|b| b.context == ctx && b.keys.chords() == pressed)
                .map(|b| b.command)
        };
        if let Some(c) = exact(context) {
            return Lookup::Command(c);
        }
        if context != Context::Global
            && let Some(c) = exact(Context::Global)
        {
            return Lookup::Command(c);
        }
        let prefix = self
            .applicable(context)
            .any(|b| b.keys.chords().len() > pressed.len() && b.keys.starts_with(pressed));
        if prefix { Lookup::Prefix } else { Lookup::None }
    }

    /// Bindings that cannot both fire: same keys in one context, or one
    /// sequence a prefix of another (the shorter would always win).
    #[must_use]
    pub fn conflicts(&self) -> Vec<Conflict> {
        let mut out: Vec<Conflict> = Vec::new();
        for ctx in Context::iter() {
            let rows: Vec<&Binding> = self.bindings.iter().filter(|b| b.context == ctx).collect();
            for (i, a) in rows.iter().enumerate() {
                for b in rows.iter().skip(i + 1) {
                    let clash = a.keys == b.keys
                        || a.keys.starts_with(b.keys.chords())
                        || b.keys.starts_with(a.keys.chords());
                    if !clash || a.command == b.command {
                        continue;
                    }
                    let keys = if a.keys.chords().len() <= b.keys.chords().len() {
                        a.keys.clone()
                    } else {
                        b.keys.clone()
                    };
                    match out.iter_mut().find(|c| c.context == ctx && c.keys == keys) {
                        Some(c) => {
                            for cmd in [a.command, b.command] {
                                if !c.commands.contains(&cmd) {
                                    c.commands.push(cmd);
                                }
                            }
                        }
                        None => out.push(Conflict {
                            context: ctx,
                            keys,
                            commands: vec![a.command, b.command],
                        }),
                    }
                }
            }
        }
        out
    }

    /// The hint bar for `context`: primary bindings of the context, then
    /// Global's, `?` last.
    #[must_use]
    pub fn hints(&self, context: Context) -> Vec<Hint> {
        let mut rows: Vec<&Binding> = self.applicable(context).filter(|b| b.primary).collect();
        rows.sort_by_key(|b| {
            (
                b.command == Command::ToggleHelp,
                b.context == Context::Global,
            )
        });
        // One key per command (the first in table order); aliases stay
        // visible in the `?` help overlay.
        let mut out: Vec<Hint> = Vec::new();
        for b in rows {
            if !out.iter().any(|h| h.command == b.command) {
                out.push(Hint {
                    keys: b.keys.to_string(),
                    command: b.command,
                    label: label(b.command).to_owned(),
                });
            }
        }
        out
    }

    /// Every binding that applies where the focus is — not just the
    /// primary ones the hint bar shows, and not one per command. A host
    /// that has to decide what a key means before the core answers (the
    /// clipboard needs the gesture that asked for it) resolves against
    /// this; anything less makes it guess, and a guess is a key doing
    /// something the core would not have done.
    #[must_use]
    pub fn applicable_bindings(&self, context: Context) -> Vec<Hint> {
        self.applicable(context)
            .map(|b| Hint {
                keys: b.keys.to_string(),
                command: b.command,
                label: label(b.command).to_owned(),
            })
            .collect()
    }

    /// The hint bar while a sequence is pending (UI-DESIGN: the zellij-style
    /// group bar): every applicable binding that starts with `pressed`,
    /// keyed by the chords still to type.
    #[must_use]
    pub fn pending_hints(&self, context: Context, pressed: &[KeyChord]) -> Vec<Hint> {
        let mut out: Vec<Hint> = Vec::new();
        for b in self.applicable(context) {
            if b.keys.chords().len() > pressed.len() && b.keys.starts_with(pressed) {
                let rest = b.keys.chords()[pressed.len()..]
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" ");
                // The same continuation may deliberately name more than
                // one command when an override conflicts. Keep both so the
                // which-key surface reports the collision instead of
                // silently making one configured command undiscoverable.
                if !out.iter().any(|h| h.keys == rest && h.command == b.command) {
                    out.push(Hint {
                        keys: rest,
                        command: b.command,
                        label: label(b.command).to_owned(),
                    });
                }
            }
        }
        out
    }

    /// One hint per bound command, for button tooltips: the first Global
    /// binding, else the first binding in any context. Never hand-written
    /// in a UI; a control without an entry here has no binding, which is
    /// a bug.
    #[must_use]
    pub fn chrome(&self) -> Vec<Hint> {
        let mut out: Vec<Hint> = Vec::new();
        let global = self
            .bindings
            .iter()
            .filter(|b| b.context == Context::Global);
        for b in global.chain(self.bindings.iter()) {
            if !out.iter().any(|h| h.command == b.command) {
                out.push(Hint {
                    keys: b.keys.to_string(),
                    command: b.command,
                    label: label(b.command).to_owned(),
                });
            }
        }
        out
    }

    /// The help overlay for `context`: all its bindings grouped by
    /// context (its own first, then Global), plus conflicts.
    #[must_use]
    pub fn help(&self, context: Context) -> HelpView {
        let group = |ctx: Context| HelpGroup {
            context: ctx,
            entries: self
                .bindings
                .iter()
                .filter(|b| b.context == ctx)
                .map(|b| HelpEntry {
                    keys: b.keys.to_string(),
                    command: b.command,
                    label: label(b.command).to_owned(),
                    primary: b.primary,
                    overridden: self.is_overridden(ctx, b.command),
                })
                .collect(),
        };
        let mut groups = vec![group(context)];
        if context != Context::Global {
            groups.push(group(Context::Global));
        }
        HelpView {
            groups,
            conflicts: self.conflicts(),
        }
    }
}

/// Human label for a command, shown in hints and help.
#[must_use]
pub fn label(command: Command) -> &'static str {
    match command {
        Command::MoveDown => "down",
        Command::MoveUp => "up",
        Command::PageDown => "page down",
        Command::PageUp => "page up",
        Command::GoTop => "top",
        Command::GoBottom => "bottom",
        Command::NextHunk => "next hunk",
        Command::PrevHunk => "previous hunk",
        Command::NextFile => "next file",
        Command::PrevFile => "previous file",
        Command::NextComment => "next comment",
        Command::PrevComment => "previous comment",
        Command::Open => "open",
        Command::Back => "back",
        Command::NextPanel => "next panel",
        Command::ToggleViewed => "mark viewed",
        Command::Comment => "comment",
        Command::ReviewFinding => "review-wide finding",
        Command::InformationalNote => "informational note",
        Command::Reply => "reply",
        Command::Delete => "delete",
        Command::ApplySuggestion => "apply suggestion",
        Command::ToggleResolved => "resolve / reopen",
        Command::DeferFinding => "defer finding",
        Command::FileSearch => "find file",
        Command::ToggleLayout => "split/unified",
        Command::ToggleWhitespace => "whitespace",
        Command::ToggleHelp => "help",
        Command::TabFiles => "files changed",
        Command::TabConversation => "conversation",
        Command::TabBrowse => "browse",
        Command::ToggleSidebar => "toggle sidebar",
        Command::Submit => "submit",
        Command::Connect => "connect",
        Command::Disconnect => "disconnect",
        Command::Commits => "commits",
        Command::Refresh => "refresh",
        Command::ScopeAll => "all changes",
        Command::ScopeByCommit => "by commit",
        Command::ScopeWorktree => "worktree",
        Command::ExpandContext => "expand context",
        Command::ContentSearch => "find in files",
        Command::ActionPalette => "actions",
        Command::CopyPath => "copy path",
        Command::CopyCheckout => "copy checkout path",
        Command::NewReview => "new review",
        Command::GoHome => "workspaces",
        Command::CopyReference => "copy reference",
        Command::NextReply => "next reply",
        Command::PrevReply => "previous reply",
        Command::CollapseParent => "collapse parent",
        Command::CollapseAll => "collapse all",
        Command::ToggleFileCollapse => "fold file",
        Command::ExpandFile => "expand full file",
        Command::FocusTree => "focus tree",
        Command::FocusDiff => "focus diff",
        Command::FocusThreads => "focus threads",
        Command::FocusRequests => "review requests",
        Command::CheckCurrent => "record current revision checked",
        Command::CheckRequested => "record requested revision checked",
        Command::CheckpointDelta => "next checkpoint delta",
        Command::FocusCommits => "focus commits",
        Command::VisualMode => "select lines",
        Command::ExpandUp => "expand up",
        Command::ExpandDown => "expand down",
        Command::CommentOnFile => "comment on file",
        Command::SideBase => "target removed side",
        Command::SideHead => "target added side",
        Command::CenterView => "centre the view",
        Command::ViewTop => "row to top",
        Command::ViewBottom => "row to bottom",
    }
}

/// Which modes a command may be bound (and run) in. One exhaustive
/// match; the config schema derives its per-mode action lists from this.
#[must_use]
pub fn modes_of(command: Command) -> &'static [Mode] {
    use Mode as M;
    match command {
        // The composer owns these; motions and Comment also run in Visual
        // (they extend / resolve the line selection); the rest is
        // Normal-only.
        Command::Submit => &[M::Insert],
        Command::Back => &[M::Normal, M::Insert, M::Visual],
        Command::MoveDown
        | Command::MoveUp
        | Command::PageDown
        | Command::PageUp
        | Command::GoTop
        | Command::GoBottom
        | Command::Comment
        | Command::ReviewFinding
        | Command::InformationalNote
        | Command::VisualMode => &[M::Normal, M::Visual],
        Command::NextHunk
        | Command::PrevHunk
        | Command::NextFile
        | Command::PrevFile
        | Command::NextComment
        | Command::PrevComment
        | Command::Open
        | Command::NextPanel
        | Command::ToggleViewed
        | Command::Reply
        | Command::Delete
        | Command::ApplySuggestion
        | Command::DeferFinding
        | Command::ToggleResolved
        | Command::FileSearch
        | Command::ToggleLayout
        | Command::ToggleWhitespace
        | Command::ToggleHelp
        | Command::TabFiles
        | Command::TabConversation
        | Command::TabBrowse
        | Command::ToggleSidebar
        | Command::Connect
        | Command::Disconnect
        | Command::Commits
        | Command::ScopeAll
        | Command::ScopeByCommit
        | Command::ScopeWorktree
        | Command::ExpandContext
        | Command::ContentSearch
        | Command::ActionPalette
        | Command::CopyPath
        | Command::CopyCheckout
        | Command::NewReview
        | Command::GoHome
        | Command::CopyReference
        | Command::NextReply
        | Command::PrevReply
        | Command::CollapseParent
        | Command::CollapseAll
        | Command::ToggleFileCollapse
        | Command::ExpandFile
        | Command::FocusTree
        | Command::FocusDiff
        | Command::FocusRequests
        | Command::CheckCurrent
        | Command::CheckRequested
        | Command::CheckpointDelta
        | Command::FocusThreads
        | Command::FocusCommits
        | Command::ExpandUp
        | Command::ExpandDown
        | Command::CommentOnFile
        | Command::SideBase
        | Command::SideHead
        | Command::CenterView
        | Command::ViewTop
        | Command::ViewBottom
        | Command::Refresh => &[M::Normal],
    }
}

/// The command's name in the config file and the `:` palette
/// (`ToggleLayout` → `toggle_layout`).
#[must_use]
pub fn config_name(command: Command) -> String {
    let name = command.to_string();
    let mut out = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// The command a config/palette name refers to (case- and
/// underscore-insensitive: `toggle_layout`, `ToggleLayout`, `togglelayout`).
#[must_use]
pub fn command_named(name: &str) -> Option<Command> {
    let norm = |s: &str| {
        s.chars()
            .filter(|c| *c != '_' && *c != '-')
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    let wanted = norm(name);
    Command::iter().find(|c| norm(&c.to_string()) == wanted)
}

/// One hint-bar entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hint {
    pub keys: String,
    pub command: Command,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelpEntry {
    pub keys: String,
    pub command: Command,
    pub label: String,
    pub primary: bool,
    pub overridden: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelpGroup {
    pub context: Context,
    pub entries: Vec<HelpEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelpView {
    pub groups: Vec<HelpGroup>,
    pub conflicts: Vec<Conflict>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_and_sequences_round_trip_through_text() {
        for text in [
            "ctrl+p",
            "g g",
            "] c",
            "shift+enter",
            "?",
            "space",
            "meta+p",
            "ctrl++",
        ] {
            let seq: KeySeq = text.parse().unwrap();
            assert_eq!(seq.to_string(), text, "{text}");
        }
        assert_eq!("".parse::<KeySeq>(), Err(KeyParseError::Empty));
        assert!(matches!(
            "hyper+x".parse::<KeySeq>(),
            Err(KeyParseError::UnknownModifier(_))
        ));
        assert!(matches!(
            "banana".parse::<KeySeq>(),
            Err(KeyParseError::UnknownKey(_))
        ));
        let json = serde_json::to_string(&"] f".parse::<KeySeq>().unwrap()).unwrap();
        assert_eq!(json, "\"] f\"");
    }

    #[test]
    fn default_table_parses_and_has_no_conflicts() {
        let map = Keymap::default_table();
        for b in map.bindings() {
            assert_ne!(
                b.keys.chords()[0],
                KeyChord::char('\0'),
                "unparsable literal for {:?}",
                b.command
            );
        }
        assert_eq!(map.conflicts(), Vec::new());
    }

    #[test]
    fn lookup_shadows_global_and_reports_prefixes() {
        let map = Keymap::default_table();
        let g = KeyChord::char('g');
        assert_eq!(map.lookup(Context::Diff, &[g]), Lookup::Prefix);
        assert_eq!(
            map.lookup(Context::Diff, &[g, g]),
            Lookup::Command(Command::GoTop)
        );
        assert_eq!(
            map.lookup(Context::Diff, &[KeyChord::char('?')]),
            Lookup::Command(Command::ToggleHelp)
        );
        // `z` opens the expand group in the diff; an unbound key is None.
        assert_eq!(
            map.lookup(Context::Diff, &[KeyChord::char('z')]),
            Lookup::Prefix
        );
        assert_eq!(
            map.lookup(Context::Diff, &[KeyChord::char('q')]),
            Lookup::None
        );
        // `c` comments in Diff and on the focused file in Tree.
        assert_eq!(
            map.lookup(Context::Diff, &[KeyChord::char('c')]),
            Lookup::Command(Command::Comment)
        );
        assert_eq!(
            map.lookup(Context::Tree, &[KeyChord::char('c')]),
            Lookup::Command(Command::Comment)
        );
    }

    #[test]
    fn overrides_replace_unbind_and_surface_conflicts() {
        let overrides: Overrides = serde_json::from_str(
            r#"{"bindings":[
                {"context":"Diff","command":"Comment","keys":"u","primary":true},
                {"context":"Diff","command":"NextHunk","keys":null},
                {"context":"Diff","command":"PrevHunk","keys":"j"}
            ]}"#,
        )
        .unwrap();
        let map = Keymap::with_overrides(&overrides);
        assert_eq!(
            map.lookup(Context::Diff, &[KeyChord::char('u')]),
            Lookup::Command(Command::Comment)
        );
        assert_eq!(
            map.lookup(Context::Diff, &[KeyChord::char('c')]),
            Lookup::None
        );
        assert_eq!(
            map.lookup(Context::Diff, &[KeyChord::char('n')]),
            Lookup::None
        );
        assert!(map.is_overridden(Context::Diff, Command::Comment));
        let conflicts = map.conflicts();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].keys.to_string(), "j");
        assert_eq!(
            conflicts[0].commands,
            vec![Command::MoveDown, Command::PrevHunk]
        );
        // Round trip of the override file.
        let text = serde_json::to_string(&overrides).unwrap();
        assert_eq!(serde_json::from_str::<Overrides>(&text).unwrap(), overrides);
    }

    #[test]
    fn help_is_non_empty_in_every_context_and_hints_are_primary_only() {
        let map = Keymap::default_table();
        for ctx in Context::iter() {
            let help = map.help(ctx);
            assert!(!help.groups[0].entries.is_empty(), "{ctx}");
            let hints = map.hints(ctx);
            assert!(!hints.is_empty(), "{ctx}");
            assert!(hints.iter().all(|h| !h.label.is_empty()));
            assert_eq!(hints.last().unwrap().command, Command::ToggleHelp);
        }
    }

    #[test]
    fn help_and_every_pending_prefix_are_exhaustively_derived_from_bindings() {
        let map = Keymap::default_table();
        for context in Context::iter() {
            let help = map.help(context);
            let contexts = if context == Context::Global {
                vec![Context::Global]
            } else {
                vec![context, Context::Global]
            };
            assert_eq!(
                help.groups.iter().map(|g| g.context).collect::<Vec<_>>(),
                contexts,
                "help groups for {context}"
            );
            for group in &help.groups {
                let bindings = map
                    .bindings()
                    .iter()
                    .filter(|binding| binding.context == group.context)
                    .collect::<Vec<_>>();
                assert_eq!(group.entries.len(), bindings.len(), "help for {context}");
                for (entry, binding) in group.entries.iter().zip(bindings) {
                    assert_eq!(entry.keys, binding.keys.to_string());
                    assert_eq!(entry.command, binding.command);
                    assert_eq!(entry.label, label(binding.command));
                    assert_eq!(entry.primary, binding.primary);
                    assert_eq!(
                        entry.overridden,
                        map.is_overridden(binding.context, binding.command)
                    );
                }
            }

            let mut prefixes = map
                .applicable(context)
                .flat_map(|binding| {
                    (1..binding.keys.chords().len())
                        .map(|len| binding.keys.chords()[..len].to_vec())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            prefixes.sort_by_key(|prefix| {
                prefix
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" ")
            });
            prefixes.dedup();
            for prefix in prefixes {
                let expected = map
                    .applicable(context)
                    .filter(|binding| {
                        binding.keys.chords().len() > prefix.len()
                            && binding.keys.starts_with(&prefix)
                    })
                    .map(|binding| Hint {
                        keys: binding.keys.chords()[prefix.len()..]
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(" "),
                        command: binding.command,
                        label: label(binding.command).to_owned(),
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    map.pending_hints(context, &prefix),
                    expected,
                    "pending hints for {context} after {}",
                    prefix
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            }
        }
    }

    #[test]
    fn overridden_conflicting_continuations_remain_discoverable() {
        let map = Keymap::with_overrides(&Overrides {
            bindings: vec![
                Override {
                    context: Context::Diff,
                    command: Command::ExpandUp,
                    keys: Some(keys!("z x")),
                    primary: false,
                },
                Override {
                    context: Context::Diff,
                    command: Command::ExpandDown,
                    keys: Some(keys!("z x")),
                    primary: false,
                },
            ],
        });
        let pending = map.pending_hints(Context::Diff, &[KeyChord::char('z')]);
        assert!(
            pending
                .iter()
                .any(|hint| hint.keys == "x" && hint.command == Command::ExpandUp)
        );
        assert!(
            pending
                .iter()
                .any(|hint| hint.keys == "x" && hint.command == Command::ExpandDown)
        );

        let help = map.help(Context::Diff);
        let diff = help
            .groups
            .iter()
            .find(|group| group.context == Context::Diff)
            .unwrap();
        for command in [Command::ExpandUp, Command::ExpandDown] {
            let entry = diff
                .entries
                .iter()
                .find(|entry| entry.command == command)
                .unwrap();
            assert_eq!(entry.keys, "z x");
            assert!(entry.overridden);
        }
        assert!(help.conflicts.iter().any(|conflict| {
            conflict.context == Context::Diff
                && conflict.keys.to_string() == "z x"
                && conflict.commands.contains(&Command::ExpandUp)
                && conflict.commands.contains(&Command::ExpandDown)
        }));
    }
}
