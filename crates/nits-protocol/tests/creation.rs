use nits_protocol::{
    CreateReviewTargets, CreateReviewTargetsError, NonEmpty, RefSpec, RepoId, ReviewTarget,
};
use proptest::prelude::*;

fn target(n: u128) -> ReviewTarget {
    ReviewTarget {
        repo_id: RepoId::from_parts(1, n),
        base: RefSpec::Head,
        head: RefSpec::WorkingTree,
    }
}

#[test]
fn creation_targets_reject_empty_and_identical_or_conflicting_duplicates() {
    assert_eq!(
        CreateReviewTargets::try_from(vec![]),
        Err(CreateReviewTargetsError::Empty)
    );
    for head in [
        RefSpec::WorkingTree,
        RefSpec::Head,
        RefSpec::Branch {
            name: "missing".into(),
        },
    ] {
        let repeated = ReviewTarget { head, ..target(1) };
        for items in [
            vec![target(1), repeated.clone()],
            vec![target(1), target(2), repeated],
        ] {
            assert_eq!(
                CreateReviewTargets::try_from(items),
                Err(CreateReviewTargetsError::Duplicate {
                    repo_id: target(1).repo_id
                })
            );
        }
    }
    let singleton = CreateReviewTargets::singleton(target(7));
    assert_eq!(singleton.as_targets().first(), &target(7));
}

proptest! {
    #[test]
    fn creation_targets_preserve_order_and_only_accept_unique_repository_ids(ids in prop::collection::vec(0_u16..30, 0..50)) {
        let items: Vec<_> = ids.iter().map(|id| target(u128::from(*id))).collect();
        let expected = !ids.is_empty() && ids.iter().collect::<std::collections::BTreeSet<_>>().len() == ids.len();
        let parsed = CreateReviewTargets::try_from(items.clone());
        prop_assert_eq!(parsed.is_ok(), expected);
        if let Ok(parsed) = parsed {
            let raw: NonEmpty<_> = parsed.into();
            prop_assert_eq!(raw.as_slice(), &items);
        }
    }
}
