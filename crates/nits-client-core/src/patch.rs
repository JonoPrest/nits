//! Section patches (plan 4.2/4.3): what a host pushes to its UI after an
//! `Effect::Render`. One patch per [`ViewSection`] carrying only that
//! section's part of the [`ViewModel`]. Viewports bound row counts, not bytes:
//! [`crate::ViewEncoder`] bounds actual IPC envelopes, including large rows,
//! previews, and attach snapshots. The UI atomically reconstructs batches and applies
//! patches to its own copy of the model; `review` (the raw open-review
//! state) is core-internal and never pushed — everything the UI shows is
//! derived into the other sections.

use nits_protocol::{
    DiffScope, ResolvedTarget, Review, ReviewId, RpcError, ViewSection, Workspace,
};
use serde::{Deserialize, Serialize};
use strum::EnumDiscriminants;

use crate::RefSelectorView;
use crate::diff::{CommitStepper, DiffView, ThreadView};
use crate::explorer::{Progress, TreeView};
use crate::focus::Focus;
use crate::keymap::{HelpView, Hint, Mode};
use crate::view::{
    ConnectionView, ContentSearchView, Draft, ScrollIntent, Tab, ViewDelta, ViewModel, ViewPrefs,
    VisualView,
};

/// The part of the view one section owns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[strum_discriminants(name(ViewPatchKind), derive(Hash, strum::EnumIter))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum ViewPatch {
    Connection {
        connection: ConnectionView,
        last_error: Option<RpcError>,
        uncertain_mutations: Vec<nits_protocol::ClientSeq>,
        daemon_management: crate::DaemonManagement,
    },
    ReviewList {
        home: crate::HomeView,
        daemon_context: Option<crate::DaemonContext>,
        active_repo: Option<nits_protocol::RepoId>,
        workspaces: Vec<Workspace>,
        reviews: Vec<Review>,
        open_review: Option<ReviewId>,
        resolved_targets: Vec<ResolvedTarget>,
        /// The open review's diff scope (UI-DESIGN §Diff scope).
        #[serde(default)]
        scope: DiffScope,
        /// The Browse tab's picked ref (UI-DESIGN §Browse).
        #[serde(default)]
        browse: Option<crate::BrowseView>,
    },
    Tree {
        tree: TreeView,
    },
    /// The diff over the viewport plus the prefs that shape it (layout,
    /// whitespace), which change together.
    Diff {
        diff: Option<DiffView>,
        diffs: Vec<DiffView>,
        prefs: ViewPrefs,
        /// The Visual-mode line selection, when it is on.
        #[serde(default)]
        visual: Option<VisualView>,
    },
    Threads {
        threads: Vec<ThreadView>,
    },
    Conversation {
        conversation: Vec<ThreadView>,
        requests: Vec<nits_protocol::ReviewRequest>,
        checkpoints: Vec<nits_protocol::ReviewerCheckpoint>,
        check_current_ready: bool,
    },
    CommitStepper {
        stepper: Option<CommitStepper>,
    },
    RefSelector {
        ref_selector: Option<RefSelectorView>,
    },
    Progress {
        progress: Progress,
    },
    Focus {
        focus: Focus,
        tab: Tab,
        /// The last `z z`/`z t`/`z b`; the host repositions the view when
        /// its `seq` changes.
        #[serde(default)]
        scroll: Option<ScrollIntent>,
        /// What `y` would copy from here; the shell copies it during the
        /// gesture that asks for it.
        #[serde(default)]
        copy_target: Option<nits_protocol::RepoPath>,
        copy_checkout: Option<String>,
        copy_reference: Option<nits_protocol::ReviewReference>,
        focused_comment: Option<nits_protocol::CommentId>,
    },
    Hints {
        hints: Vec<Hint>,
        pending: String,
        pending_label: Option<String>,
        mode: Mode,
        leader: String,
        chrome: Vec<Hint>,
        /// Every binding applicable where the focus is (§6.4).
        #[serde(default)]
        bindings: Vec<Hint>,
        /// What the core made of the last key it acted on, for a host
        /// that must act before it answers (§6.4).
        #[serde(default)]
        last_key: Option<crate::view::LastKey>,
    },
    Help {
        help: Option<HelpView>,
    },
    Draft {
        draft: Option<Draft>,
        pending_refresh: bool,
    },
    /// The palettes (UI-DESIGN §Search): content search and actions.
    Search {
        content_search: Option<ContentSearchView>,
        action_palette: bool,
    },
}

impl ViewPatch {
    #[must_use]
    pub fn section(&self) -> ViewSection {
        match self {
            ViewPatch::Connection { .. } => ViewSection::Connection,
            ViewPatch::ReviewList { .. } => ViewSection::ReviewList,
            ViewPatch::Tree { .. } => ViewSection::Tree,
            ViewPatch::Diff { .. } => ViewSection::Diff,
            ViewPatch::Threads { .. } => ViewSection::Threads,
            ViewPatch::Conversation { .. } => ViewSection::Conversation,
            ViewPatch::CommitStepper { .. } => ViewSection::CommitStepper,
            ViewPatch::RefSelector { .. } => ViewSection::RefSelector,
            ViewPatch::Progress { .. } => ViewSection::Progress,
            ViewPatch::Focus { .. } => ViewSection::Focus,
            ViewPatch::Hints { .. } => ViewSection::Hints,
            ViewPatch::Help { .. } => ViewSection::Help,
            ViewPatch::Draft { .. } => ViewSection::Draft,
            ViewPatch::Search { .. } => ViewSection::Search,
        }
    }
}

impl ViewModel {
    /// The patch that carries `section` of this model.
    #[must_use]
    pub fn patch(&self, section: ViewSection) -> ViewPatch {
        match section {
            ViewSection::Connection => ViewPatch::Connection {
                connection: self.connection.clone(),
                last_error: self.last_error.clone(),
                uncertain_mutations: self.uncertain_mutations.clone(),
                daemon_management: self.daemon_management.clone(),
            },
            ViewSection::ReviewList => ViewPatch::ReviewList {
                home: self.home.clone(),
                daemon_context: self.daemon_context.clone(),
                active_repo: self.active_repo,
                workspaces: self.workspaces.clone(),
                reviews: self.reviews.clone(),
                open_review: self.open_review,
                resolved_targets: self.resolved_targets.clone(),
                scope: self.scope,
                browse: self.browse.clone(),
            },
            ViewSection::Tree => ViewPatch::Tree {
                tree: self.tree.clone(),
            },
            ViewSection::Diff => ViewPatch::Diff {
                diff: self.diff.clone(),
                diffs: self.diffs.clone(),
                prefs: self.prefs,
                visual: self.visual,
            },
            ViewSection::Threads => ViewPatch::Threads {
                threads: self.threads.clone(),
            },
            ViewSection::Conversation => ViewPatch::Conversation {
                conversation: self.conversation.clone(),
                requests: self.requests.clone(),
                checkpoints: self.checkpoints.clone(),
                check_current_ready: self.check_current_ready,
            },
            ViewSection::CommitStepper => ViewPatch::CommitStepper {
                stepper: self.stepper.clone(),
            },
            ViewSection::RefSelector => ViewPatch::RefSelector {
                ref_selector: self.ref_selector.clone(),
            },
            ViewSection::Progress => ViewPatch::Progress {
                progress: self.progress,
            },
            ViewSection::Focus => ViewPatch::Focus {
                focus: self.focus,
                tab: self.tab,
                scroll: self.scroll,
                copy_target: self.copy_target.clone(),
                copy_checkout: self.copy_checkout.clone(),
                copy_reference: self.copy_reference.clone(),
                focused_comment: self.focused_comment,
            },
            ViewSection::Hints => ViewPatch::Hints {
                hints: self.hints.clone(),
                pending: self.pending_keys.clone(),
                pending_label: self.pending_label.clone(),
                mode: self.mode,
                leader: self.leader.clone(),
                chrome: self.chrome.clone(),
                bindings: self.bindings.clone(),
                last_key: self.last_key,
            },
            ViewSection::Help => ViewPatch::Help {
                help: self.help.clone(),
            },
            ViewSection::Draft => ViewPatch::Draft {
                draft: self.draft.clone(),
                pending_refresh: self.pending_refresh,
            },
            ViewSection::Search => ViewPatch::Search {
                content_search: self.content_search.clone(),
                action_palette: self.action_palette,
            },
        }
    }

    /// Patches for every section a render delta names, in delta order.
    #[must_use]
    pub fn patches(&self, delta: &ViewDelta) -> Vec<ViewPatch> {
        delta.sections.iter().map(|s| self.patch(*s)).collect()
    }

    /// Every section, for a UI that just attached.
    #[must_use]
    pub fn full_patches(&self) -> Vec<ViewPatch> {
        use strum::IntoEnumIterator;
        ViewSection::iter().map(|s| self.patch(s)).collect()
    }

    /// Install a patch into this model (the UI-side copy).
    // A flat exhaustive field transfer keeps the section contract visible.
    #[allow(clippy::too_many_lines)]
    pub fn apply(&mut self, patch: ViewPatch) {
        match patch {
            ViewPatch::Connection {
                connection,
                last_error,
                uncertain_mutations,
                daemon_management,
            } => {
                self.connection = connection;
                self.last_error = last_error;
                self.uncertain_mutations = uncertain_mutations;
                self.daemon_management = daemon_management;
            }
            ViewPatch::ReviewList {
                home,
                daemon_context,
                active_repo,
                workspaces,
                reviews,
                open_review,
                resolved_targets,
                scope,
                browse,
            } => {
                self.home = home;
                self.daemon_context = daemon_context;
                self.active_repo = active_repo;
                self.workspaces = workspaces;
                self.reviews = reviews;
                self.open_review = open_review;
                self.resolved_targets = resolved_targets;
                self.scope = scope;
                self.browse = browse;
            }
            ViewPatch::Tree { tree } => self.tree = tree,
            ViewPatch::Diff {
                diff,
                diffs,
                prefs,
                visual,
            } => {
                self.diff = diff;
                self.diffs = diffs;
                self.prefs = prefs;
                self.visual = visual;
            }
            ViewPatch::Threads { threads } => self.threads = threads,
            ViewPatch::Conversation {
                conversation,
                requests,
                checkpoints,
                check_current_ready,
            } => {
                self.conversation = conversation;
                self.requests = requests;
                self.checkpoints = checkpoints;
                self.check_current_ready = check_current_ready;
            }
            ViewPatch::CommitStepper { stepper } => self.stepper = stepper,
            ViewPatch::RefSelector { ref_selector } => self.ref_selector = ref_selector,
            ViewPatch::Progress { progress } => self.progress = progress,
            ViewPatch::Focus {
                focus,
                tab,
                scroll,
                copy_target,
                copy_checkout,
                copy_reference,
                focused_comment,
            } => {
                self.focus = focus;
                self.tab = tab;
                self.scroll = scroll;
                self.copy_target = copy_target;
                self.copy_checkout = copy_checkout;
                self.copy_reference = copy_reference;
                self.focused_comment = focused_comment;
            }
            ViewPatch::Hints {
                hints,
                pending,
                pending_label,
                mode,
                leader,
                chrome,
                bindings,
                last_key,
            } => {
                self.bindings = bindings;
                self.last_key = last_key;
                self.hints = hints;
                self.pending_keys = pending;
                self.pending_label = pending_label;
                self.mode = mode;
                self.leader = leader;
                self.chrome = chrome;
            }
            ViewPatch::Help { help } => self.help = help,
            ViewPatch::Draft {
                draft,
                pending_refresh,
            } => {
                self.draft = draft;
                self.pending_refresh = pending_refresh;
            }
            ViewPatch::Search {
                content_search,
                action_palette,
            } => {
                self.content_search = content_search;
                self.action_palette = action_palette;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strum::IntoEnumIterator;

    #[test]
    fn every_section_has_a_patch_and_full_patches_rebuild_the_view() {
        let source = ViewModel {
            pending_refresh: true,
            focus: Focus::Help,
            progress: Progress {
                viewed: 1,
                changed_since_viewed: 2,
                total: 3,
                additions: 4,
                deletions: 5,
            },
            connection: ConnectionView::Subscribed,
            ..ViewModel::default()
        };
        for s in ViewSection::iter() {
            assert_eq!(source.patch(s).section(), s);
        }
        let mut copy = ViewModel::default();
        for p in source.full_patches() {
            copy.apply(p);
        }
        // `review` is never pushed; everything else arrives.
        assert_eq!(copy.review, None);
        copy.review.clone_from(&source.review);
        assert_eq!(copy, source);
    }
}
