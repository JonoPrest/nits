//! `Request` → `Core` calls. Single-response requests return a `Response`;
//! `open_review` streams straight into the connection's outbox.

use std::sync::Arc;

use nits_protocol::{
    ChunkIndex, EntityKind, Mutation, RenderOpts, RenderTarget, Request, RequestId, Response,
    ReviewId, ServerMsg, StreamItem,
};
use nits_review_core::{Core, CoreError};

use crate::connection::{Outbox, send_chunk};
use crate::daemon::{Daemon, DaemonError};
use crate::handshake::Negotiated;

/// Handle every request whose shape is `Single`.
///
/// Streaming requests are matched by the connection before this is called;
/// reaching them here is a programming error reported as `Internal`.
// One arm per request; splitting would hide the exhaustive match.
#[allow(clippy::too_many_lines)]
pub async fn single(
    daemon: &Arc<Daemon>,
    who: &Negotiated,
    request: Request,
) -> Result<Response, DaemonError> {
    match request {
        Request::EnsureDirectoryReview {
            client_seq,
            options,
        } => {
            let ctx = Daemon::ctx(who.author.clone(), who.client_id, client_seq);
            let (review, _) = daemon
                .write(move |core| core.ensure_directory_review(&ctx, options))
                .await?;
            Ok(Response::DirectoryReview { review })
        }
        Request::ListWorkspaces => {
            let workspaces = daemon.read(Core::workspaces).await?;
            Ok(Response::Workspaces { workspaces })
        }
        Request::ListReviews { workspace_id } => {
            let reviews = daemon.read(move |c| c.reviews(workspace_id)).await?;
            Ok(Response::Reviews { reviews })
        }
        Request::DefaultBase { repo_id } => {
            let base = daemon.read(move |c| c.default_base(repo_id)).await?;
            Ok(Response::DefaultBase { base })
        }
        Request::ListRefs { repo_id } => {
            let refs = daemon.read(move |c| c.ref_candidates(repo_id)).await?;
            Ok(Response::Refs { repo_id, refs })
        }
        Request::GetReview { review_id } => {
            let review = daemon.read(move |c| c.review(review_id)).await?.review;
            Ok(Response::Review { review })
        }
        Request::ResolveTargets { review_id } => {
            let ctx = Daemon::ctx(
                who.author.clone(),
                who.client_id,
                nits_protocol::ClientSeq::new(0),
            );
            let ((targets, changed), _) = daemon
                .write(move |c| c.resolve_targets(&ctx, review_id))
                .await?;
            Ok(Response::Resolved { targets, changed })
        }
        Request::ListCommits { review_id, repo_id } => {
            let commits = daemon.read(move |c| c.commits(review_id, repo_id)).await?;
            Ok(Response::Commits { commits })
        }
        Request::ReviewSnapshot { review_id } => {
            let snapshot = daemon.read(move |c| c.review_snapshot(review_id)).await?;
            Ok(Response::ReviewSnapshot { snapshot })
        }
        Request::ListFiles { review_id, scope } => {
            let (files, resolved) = daemon
                .read(move |c| c.files_scoped(review_id, &scope))
                .await?;
            Ok(Response::Files {
                files,
                resolved: resolved.into_iter().collect(),
            })
        }
        Request::Search {
            review_id,
            query,
            all_files,
            scope,
        } => {
            let (hits, truncated) = daemon
                .read(move |c| c.search(review_id, &query, all_files, &scope))
                .await?;
            Ok(Response::Search { hits, truncated })
        }
        Request::TreeSnapshot { repo_id, ref_spec } => {
            let snapshot = daemon
                .read(move |c| c.tree_snapshot(repo_id, &ref_spec))
                .await?;
            Ok(Response::TreeSnapshot { snapshot })
        }
        Request::RenderChunk {
            repo_id,
            path,
            target,
            opts,
            index,
        } => {
            let chunk = daemon
                .read(move |c| {
                    let (_, rendered) = match target {
                        RenderTarget::Diff { change } => {
                            c.render_change(repo_id, &path, change, opts)?
                        }
                        RenderTarget::Blob { oid } => c.blob_render(repo_id, &path, oid)?,
                    };
                    rendered.chunk(index).ok_or_else(|| CoreError::NotFound {
                        kind: EntityKind::Chunk,
                        id: index.get().to_string(),
                    })
                })
                .await?;
            Ok(Response::RenderChunk { chunk })
        }
        Request::Mutate {
            client_seq,
            mutation,
        } => {
            let ctx = Daemon::ctx(who.author.clone(), who.client_id, client_seq);
            let ((), events) = daemon.write(move |c| apply(c, &ctx, mutation)).await?;
            // A mutation may append follow-up events (for example a target
            // update is followed by resolved targets).  Acknowledge the
            // mutation's primary event so the client cannot advance past it
            // before the broadcast tail delivers the sequence in order.
            let event = events.into_iter().next().ok_or_else(|| {
                DaemonError::Core(CoreError::Invalid {
                    reason: "mutation committed no event".into(),
                })
            })?;
            Ok(Response::Committed { event })
        }
        Request::Subscribe { .. }
        | Request::Unsubscribe { .. }
        | Request::Shutdown
        | Request::OpenReview { .. }
        | Request::FileRender { .. }
        | Request::ChangeRender { .. }
        | Request::BlobRender { .. } => Err(DaemonError::Core(CoreError::Invalid {
            reason: "request routed to the wrong handler".into(),
        })),
    }
}

#[allow(clippy::too_many_lines)] // one exhaustive mapping from each mutation to Core
fn apply(core: &Core, ctx: &nits_review_core::Ctx, m: Mutation) -> Result<(), CoreError> {
    match m {
        Mutation::CreateWorkspace { workspace_id, name } => {
            core.create_workspace(ctx, workspace_id, name)?;
        }
        Mutation::RenameWorkspace { workspace_id, name } => {
            core.rename_workspace(ctx, workspace_id, name)?;
        }
        Mutation::AttachRepo {
            workspace_id,
            repo_id,
            path,
            display_name,
        } => {
            core.attach_repo(ctx, workspace_id, repo_id, &path, display_name)?;
        }
        Mutation::DetachRepo {
            workspace_id,
            repo_id,
        } => core.detach_repo(ctx, workspace_id, repo_id)?,
        Mutation::CreateReview {
            review_id,
            workspace_id,
            title,
            targets,
        } => {
            core.create_review(ctx, review_id, workspace_id, title, targets)?;
        }
        Mutation::UpdateReview {
            review_id,
            title,
            status,
        } => core.update_review(ctx, review_id, title, status)?,
        Mutation::UpdateReviewTarget { review_id, update } => {
            core.update_review_target(ctx, review_id, update)?;
        }
        Mutation::DeleteReview { review_id } => core.delete_review(ctx, review_id)?,
        Mutation::AddComment {
            review_id,
            comment_id,
            kind,
            anchor,
            body,
            context,
        } => {
            core.add_comment(ctx, review_id, comment_id, kind, anchor, body, context)?;
        }
        Mutation::Reply {
            review_id,
            thread_id,
            comment_id,
            kind,
            body,
        } => {
            core.reply(ctx, review_id, thread_id, comment_id, kind, body)?;
        }
        Mutation::EditComment {
            review_id,
            comment_id,
            body,
        } => core.edit_comment(ctx, review_id, comment_id, body)?,
        Mutation::DeleteComment {
            review_id,
            comment_id,
        } => core.delete_comment(ctx, review_id, comment_id)?,
        Mutation::DeferThread {
            review_id,
            thread_id,
            reason,
            tracking_url,
        } => core.defer_thread(ctx, review_id, thread_id, reason, tracking_url)?,
        Mutation::ResolveThread {
            review_id,
            thread_id,
        } => core.resolve_thread(ctx, review_id, thread_id)?,
        Mutation::UnresolveThread {
            review_id,
            thread_id,
        } => core.unresolve_thread(ctx, review_id, thread_id)?,
        Mutation::MarkViewed {
            review_id,
            repo_id,
            path,
        } => {
            core.mark_viewed(ctx, review_id, repo_id, path)?;
        }
        Mutation::UnmarkViewed {
            review_id,
            repo_id,
            path,
        } => core.unmark_viewed(ctx, review_id, repo_id, path)?,
        Mutation::RequestReview {
            review_id,
            agent,
            note,
        } => {
            core.request_review(ctx, review_id, agent, note)?;
        }
        Mutation::RecordCheckpoint {
            review_id,
            targets,
            in_reply_to,
        } => {
            core.record_checkpoint(ctx, review_id, targets, in_reply_to)?;
        }
        Mutation::ApplySuggestion {
            review_id,
            comment_id,
        } => {
            core.apply_suggestion(ctx, review_id, comment_id)?;
        }
    }
    Ok(())
}

/// The §4.8 open stream: snapshot → tree per target ref → header per changed
/// file → first chunk per file. Renders run one at a time on the blocking
/// pool; headers are sent as each finishes so the client can build its tree
/// while later files are still rendering.
pub async fn open_review(
    daemon: &Arc<Daemon>,
    id: RequestId,
    out: &Outbox,
    review_id: ReviewId,
    opts: RenderOpts,
) -> Result<(), DaemonError> {
    let snapshot = daemon.read(move |c| c.review_snapshot(review_id)).await?;
    let resolved = snapshot.resolved.clone();
    out.send(ServerMsg::StreamItem {
        id,
        item: StreamItem::ReviewSnapshot { snapshot },
    });
    let Some(resolved) = resolved else {
        // Targets never resolved: nothing to render yet.
        return Ok(());
    };
    for t in resolved.iter().cloned() {
        for r in [t.base, t.head] {
            let snapshot = daemon
                .read(move |c| c.review_tree_snapshot(review_id, t.repo_id, &r))
                .await?;
            out.send(ServerMsg::StreamItem {
                id,
                item: StreamItem::TreeSnapshot { snapshot },
            });
        }
    }
    let files = daemon.read(move |c| c.files(review_id)).await?;
    let mut first_chunks = Vec::with_capacity(files.len());
    for f in files {
        let opts = opts.clone();
        let (header, rendered) = daemon
            .read(move |c| c.render_change(f.repo_id, &f.path, f.kind, opts))
            .await?;
        first_chunks.push((
            header.repo_id,
            header.path.clone(),
            rendered.chunk(ChunkIndex::FIRST),
        ));
        out.send(ServerMsg::StreamItem {
            id,
            item: StreamItem::Header { header },
        });
    }
    for (repo_id, path, chunk) in first_chunks {
        if let Some(chunk) = chunk {
            send_chunk(out, id, repo_id, path, chunk);
        }
    }
    Ok(())
}
