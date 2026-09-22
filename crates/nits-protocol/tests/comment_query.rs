use nits_protocol::{CommentQuery, Request, Response, ResponseShape, ReviewId, StreamItem};
use serde_json::json;

#[test]
fn comment_filters_parse_once_and_reject_invalid_ids_paths_status_and_cursors() {
    assert_eq!(
        serde_json::from_value::<CommentQuery>(json!({})).unwrap(),
        CommentQuery::default()
    );
    for invalid in [
        json!({"status":"open"}),
        json!({"status":"Closed"}),
        json!({"thread_id":"wrong"}),
        json!({"repo_id":"wrong"}),
        json!({"path":"../outside"}),
        json!({"path":"/absolute"}),
        json!({"since":-1}),
        json!({"since":1.5}),
        json!({"author":3}),
        json!({"open":true}),
    ] {
        assert!(
            serde_json::from_value::<CommentQuery>(invalid.clone()).is_err(),
            "{invalid}"
        );
    }
    let query: CommentQuery =
        serde_json::from_value(json!({"since":u64::MAX,"status":"Deleted"})).unwrap();
    assert_eq!(query.since.unwrap().get(), u64::MAX);
    assert_eq!(
        Request::ListComments {
            review_id: ReviewId::nil(),
            query
        }
        .shape(),
        ResponseShape::Single
    );
}

#[test]
fn comment_listing_has_an_empty_coherent_cursor_and_is_not_a_stream_item() {
    let wire = json!({"type":"CommentListing","listing":{
        "threads":[],"summary":{"threads":0,"open":0,"resolved":0,"deferred":0,"informational":0,"deleted":0,"comments":0,"deleted_comments":0},
        "suggestions":[],"requests":[],"checkpoints":[],"latest_checkpoints":[],"seq":42
    }});
    assert!(serde_json::from_value::<Response>(wire.clone()).is_ok());
    assert!(serde_json::from_value::<StreamItem>(wire).is_err());
}
