use nits_protocol::{
    Request, Response, ResponseShape, ReviewQuery, ReviewScope, StreamItem, WorkspaceId,
};
use serde_json::json;

#[test]
fn discovery_scope_defaults_to_all_and_rejects_ambiguous_or_malformed_boundaries() {
    assert_eq!(
        serde_json::from_value::<ReviewQuery>(json!({})).unwrap(),
        ReviewQuery::default()
    );
    let workspace = WorkspaceId::from_parts(1, 7);
    let scoped = json!({"scope":{"type":"Workspace","workspace_id":workspace},"title":"PR #247","awaiting":"reader"});
    let parsed: ReviewQuery = serde_json::from_value(scoped).unwrap();
    assert_eq!(
        parsed.scope,
        ReviewScope::Workspace {
            workspace_id: workspace
        }
    );
    for bad in [
        json!({"scope":{"type":"All","workspace_id":workspace}}),
        json!({"scope":{"type":"Workspace"}}),
        json!({"scope":{"type":"Workspace","workspace_id":"not-an-id"}}),
        json!({"workspace_id":workspace}),
        json!({"title":7}),
        json!({"awaiting":[]}),
    ] {
        assert!(
            serde_json::from_value::<ReviewQuery>(bad.clone()).is_err(),
            "{bad}"
        );
    }
    let request = Request::DiscoverReviews { query: parsed };
    assert_eq!(request.shape(), ResponseShape::Single);
}

#[test]
fn discovery_is_a_single_response_with_a_coherent_empty_watermark() {
    let wire = json!({"type":"ReviewDiscovery","discovery":{"reviews":[],"seq":42}});
    assert!(serde_json::from_value::<Response>(wire.clone()).is_ok());
    assert!(serde_json::from_value::<StreamItem>(wire).is_err());
}
